use std::collections::HashMap;
use std::io::{self, Write};

use crate::cache::{Packed, Reader, invalid};
use crate::tokenizer::{is_separator, normalize, normalized_tokens};
use crate::word_set::WordSet;
use crate::{java_trim, lowercase};

// Three bytes per node and per edge leave room for 8 Mi nodes and 16 Mi token IDs.
type TrieArray = Packed<3>;
const TERMINAL: u32 = 1 << 23;
const EDGE_OFFSET: u32 = TERMINAL - 1;

/// A case-insensitive matcher for GROBID gazetteer phrases.
///
/// Nodes are in breadth-first order so edge index `i` leads to node `i + 1`.
/// Each node stores its first outgoing edge and a terminal bit. Edges store only
/// token IDs, sorted within each node's range for binary search.
pub struct FastMatcher {
    vocabulary: WordSet,
    nodes: TrieArray,
    edges: TrieArray,
}

impl Default for FastMatcher {
    fn default() -> Self {
        Self {
            vocabulary: WordSet::default(),
            nodes: TrieArray::zeros(2),
            edges: TrieArray::default(),
        }
    }
}

impl FastMatcher {
    /// Builds a matcher from phrases as they would appear in a GROBID gazetteer.
    pub fn from_phrases(phrases: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut builder = MatcherBuilder::default();
        for phrase in phrases {
            builder.add(phrase.as_ref());
        }
        builder.finish()
    }

    fn node(&self, index: usize) -> u32 {
        self.nodes.get(index)
    }

    fn child(&self, node: usize, token: u32) -> Option<usize> {
        let mut low = (self.node(node) & EDGE_OFFSET) as usize;
        let mut high = (self.node(node + 1) & EDGE_OFFSET) as usize;
        while low < high {
            let mid = low + (high - low) / 2;
            match self.edges.get(mid).cmp(&token) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some(mid + 1),
            }
        }
        None
    }

    fn token_id(&self, token: &str) -> Option<u32> {
        self.vocabulary.find(&lowercase(token))
    }

    fn for_each_match(&self, tokens: &[&str], mut emit: impl FnMut(usize, usize)) {
        if self.edges.len() == 0 {
            return;
        }

        let mut active: Vec<(usize, usize)> = Vec::new();
        for (end, &token) in tokens.iter().enumerate() {
            let token = normalize(token);
            if is_separator(&token) || token.chars().all(char::is_whitespace) {
                continue;
            }
            let Some(id) = self.token_id(&token) else {
                active.clear();
                continue;
            };

            active.retain_mut(|(start, node)| {
                let Some(child) = self.child(*node, id) else {
                    return false;
                };
                *node = child;
                if self.node(child) & TERMINAL != 0 {
                    emit(*start, end);
                }
                true
            });

            if let Some(child) = self.child(0, id) {
                if self.node(child) & TERMINAL != 0 {
                    emit(end, end);
                }
                active.push((end, child));
            }
        }
    }

    /// Returns all matches as inclusive input token ranges, ordered by end and then start.
    /// Prefixes and overlapping matches are included. Separators inside a span retain their indices.
    pub fn match_tokens(&self, tokens: &[&str]) -> Vec<(usize, usize)> {
        let mut matches = Vec::new();
        self.for_each_match(tokens, |start, end| matches.push((start, end)));
        matches
    }

    /// Marks the input tokens covered by at least one match.
    pub fn match_positions(&self, tokens: &[&str]) -> Vec<bool> {
        let mut positions = vec![false; tokens.len()];
        self.for_each_match(tokens, |start, end| positions[start..=end].fill(true));
        positions
    }

    pub(crate) fn contains_phrase(&self, phrase: &str) -> bool {
        let text = normalize(phrase);
        let mut node = 0;
        for (token, _) in normalized_tokens(&text).filter(|&(token, _)| !is_separator(token)) {
            let Some(child) = self.token_id(token).and_then(|id| self.child(node, id)) else {
                return false;
            };
            node = child;
        }
        self.node(node) & TERMINAL != 0
    }

    pub(crate) fn read(reader: &mut Reader) -> io::Result<Self> {
        let vocabulary = WordSet::read(reader)?;
        let nodes = TrieArray::read(reader)?;
        let edges = TrieArray::read(reader)?;
        if nodes.len() < 2
            || nodes.len() - 2 != edges.len()
            || nodes.get(0) != 0
            || nodes.get(nodes.len() - 1) as usize != edges.len()
        {
            return Err(invalid("invalid trie dimensions"));
        }

        // In breadth-first order a child always has a greater index than its parent.
        let mut start = 0;
        for (node, value) in nodes.iter().skip(1).enumerate() {
            let end = (value & EDGE_OFFSET) as usize;
            if end < start || end > edges.len() || (end > start && start < node) {
                return Err(invalid("invalid trie edge range"));
            }
            if end - start > 1 && !edges.is_strictly_sorted(start..end) {
                return Err(invalid("unsorted trie token IDs"));
            }
            start = end;
        }
        if edges.iter().any(|id| id as usize >= vocabulary.len()) {
            return Err(invalid("invalid trie token ID"));
        }
        Ok(Self {
            vocabulary,
            nodes,
            edges,
        })
    }

    pub(crate) fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        self.vocabulary.write(writer)?;
        self.nodes.write(writer)?;
        self.edges.write(writer)
    }
}

const NONE: u32 = u32::MAX;

struct BuildNode {
    first_edge: u32,
    terminal: bool,
}

impl Default for BuildNode {
    fn default() -> Self {
        Self {
            first_edge: NONE,
            terminal: false,
        }
    }
}

struct BuildEdge {
    token: u32,
    next_edge: u32,
}

pub(crate) struct MatcherBuilder {
    vocabulary: WordSet,
    nodes: Vec<BuildNode>,
    edges: Vec<BuildEdge>,
    transitions: HashMap<(u32, u32), u32>,
}

impl Default for MatcherBuilder {
    fn default() -> Self {
        Self {
            vocabulary: WordSet::default(),
            nodes: vec![BuildNode::default()],
            edges: Vec::new(),
            transitions: HashMap::new(),
        }
    }
}

impl MatcherBuilder {
    pub(crate) fn add(&mut self, phrase: &str) {
        let normalized = normalize(phrase);
        let phrase = java_trim(&normalized);
        // Apache StringUtils.normalizeSpace treats these control characters as whitespace.
        let cleaned;
        let phrase = if phrase.contains(['\x1c', '\x1d', '\x1e', '\x1f']) {
            cleaned = phrase.replace(['\x1c', '\x1d', '\x1e', '\x1f'], " ");
            cleaned.as_str()
        } else {
            phrase
        };

        let phrase = lowercase(phrase);
        let mut node = 0;
        for (token, _) in normalized_tokens(&phrase).filter(|&(token, _)| !is_separator(token)) {
            let id = self.vocabulary.intern(token);
            node = *self.transitions.entry((node, id)).or_insert_with(|| {
                let parent = &mut self.nodes[node as usize];
                let edge = self.edges.len() as u32;
                assert!(
                    edge < EDGE_OFFSET,
                    "lexicon trie exceeds cache format limits"
                );
                self.edges.push(BuildEdge {
                    token: id,
                    next_edge: parent.first_edge,
                });
                parent.first_edge = edge;
                self.nodes.push(BuildNode::default());
                edge + 1
            });
        }
        if node != 0 {
            self.nodes[node as usize].terminal = true;
        }
    }

    pub(crate) fn finish(self) -> FastMatcher {
        let Self {
            mut vocabulary,
            nodes: build_nodes,
            edges: build_edges,
            transitions,
        } = self;
        drop(transitions);
        vocabulary.shrink_to_fit();

        let mut nodes = TrieArray::zeros(build_nodes.len() + 1);
        let mut edges = TrieArray::zeros(build_edges.len());
        let mut order = Vec::with_capacity(build_nodes.len());
        let mut children = Vec::new();
        order.push(0);
        for index in 0..build_nodes.len() {
            let node = &build_nodes[order[index] as usize];
            let start = order.len() - 1;
            nodes.set(
                index,
                start as u32 | if node.terminal { TERMINAL } else { 0 },
            );
            let mut edge = node.first_edge;
            while edge != NONE {
                let entry = &build_edges[edge as usize];
                children.push((entry.token, edge + 1));
                edge = entry.next_edge;
            }
            children.sort_unstable_by_key(|&(token, _)| token);
            for (offset, (token, child)) in children.drain(..).enumerate() {
                edges.set(start + offset, token);
                order.push(child);
            }
        }
        nodes.set(build_nodes.len(), edges.len() as u32);
        FastMatcher {
            vocabulary,
            nodes,
            edges,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_prefixes_and_overlaps_with_original_indices() {
        let matcher =
            FastMatcher::from_phrases(["New", "New York", "York", "York–Times", "N.E.J.M."]);
        let tokens = ["(", "NEW", "-", "York", " ", "Times", ".", "now"];
        assert_eq!(
            matcher.match_tokens(&tokens),
            [(1, 1), (1, 3), (3, 3), (3, 5)]
        );
        assert_eq!(
            matcher.match_positions(&tokens),
            [false, true, true, true, true, true, false, false]
        );
        assert!(matcher.contains_phrase("New York"));
        assert!(matcher.contains_phrase("N.E.J.M."));
        assert!(!matcher.contains_phrase("New York Times"));
    }

    #[test]
    fn ignores_empty_phrases_and_resets_on_unknown_words() {
        let matcher = FastMatcher::from_phrases(["", "...", " a b ", "École"]);
        assert_eq!(
            matcher.match_tokens(&["a", "unknown", "b", "ÉCOLE"]),
            [(3, 3)]
        );
        assert!(matcher.match_tokens(&["..."]).is_empty());
        assert!(FastMatcher::default().match_tokens(&["a"]).is_empty());
    }
}
