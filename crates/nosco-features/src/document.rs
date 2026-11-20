use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use nosco_lexicon::{Lexicon, tokenizer};
use nosco_pdf::{GraphicKind, GraphicObject, LayoutDocument, LayoutToken, PageInfo};
use regex::Regex;

use crate::factory::*;
use crate::text::{address_flags, has_layout_space, layout_parts, match_flags};
use crate::{
    BLOCK, FeatureMatrix, LINE, boundary, clean_token, columns, filtered, flag, lexical_columns,
    token_classes,
};

/// A segmentation row maps to one index in `LayoutDocument::lines`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SegmentationInput {
    pub features: FeatureMatrix,
    pub line_indices: Vec<usize>,
}

/// Reference segmentation predicts one label per token. The line index is
/// repeated for every emitted token on that line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefSegmenterInput {
    pub features: FeatureMatrix,
    pub line_indices: Vec<usize>,
    pub token_indices: Vec<usize>,
}

/// Token mapping for fulltext rows, including rows reused by figure and table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FulltextInput {
    pub features: FeatureMatrix,
    pub token_indices: Vec<usize>,
}

impl FulltextInput {
    /// Pass a cluster's indices to recover the original body features used by
    /// both the figure and table parsers.
    pub fn select(&self, token_indices: &[usize]) -> FeatureMatrix {
        if self.token_indices.is_sorted() {
            return FeatureMatrix {
                rows: token_indices
                    .iter()
                    .filter_map(|i| {
                        self.token_indices
                            .binary_search(i)
                            .ok()
                            .and_then(|position| self.features.rows.get(position))
                            .cloned()
                    })
                    .collect(),
            };
        }
        let by_token: HashMap<_, _> = self
            .token_indices
            .iter()
            .copied()
            .zip(&self.features.rows)
            .collect();
        FeatureMatrix {
            rows: token_indices
                .iter()
                .filter_map(|i| by_token.get(i).map(|row| (*row).clone()))
                .collect(),
        }
    }
}

/// Bibliography metadata used for the two fulltext callout features.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReferenceCallout<'a> {
    pub label: Option<&'a str>,
    pub first_author: Option<&'a str>,
}

const PAGE: [&str; 3] = ["PAGESTART", "PAGEIN", "PAGEEND"];

#[derive(Default)]
struct FontState<'a> {
    previous: Option<&'a LayoutToken>,
}

impl<'a> FontState<'a> {
    fn columns(&mut self, token: &'a LayoutToken) -> [&'static str; 4] {
        let font = if self
            .previous
            .is_some_and(|prev| prev.font_name == token.font_name)
        {
            "SAMEFONT"
        } else {
            "NEWFONT"
        };
        let size = match self
            .previous
            .map(|prev| (prev.font_size as i32).cmp(&(token.font_size as i32)))
        {
            Some(Ordering::Equal) => "SAMEFONTSIZE",
            Some(Ordering::Greater) => "LOWERFONT",
            _ => "HIGHERFONT",
        };
        self.previous = Some(token);
        [font, size, flag(token.bold), flag(token.italic)]
    }
}

#[derive(Default)]
struct Alignment {
    x: Option<f64>,
    indented: bool,
}

impl Alignment {
    fn update(&mut self, token: &LayoutToken, text: &str) {
        if let Some(previous) = self.x {
            let width = token.bbox.width / utf16_len(text).max(1) as f64;
            let dx = token.bbox.x - previous;
            if dx.abs() > width {
                self.indented = dx > 0.0;
            }
        }
        self.x = Some(token.bbox.x);
    }

    fn label(&self) -> &'static str {
        if self.indented {
            "LINEINDENT"
        } else {
            "ALIGNEDLEFT"
        }
    }
}

struct Geometry<'a> {
    pages: HashMap<usize, &'a PageInfo>,
    graphics: HashMap<usize, Vec<&'a GraphicObject>>,
}

impl<'a> Geometry<'a> {
    fn new(doc: &'a LayoutDocument) -> Self {
        let pages = doc.pages.iter().map(|page| (page.number, page)).collect();
        let mut graphics: HashMap<_, Vec<_>> = HashMap::new();
        for graphic in &doc.graphics {
            graphics.entry(graphic.page).or_default().push(graphic);
        }
        Self { pages, graphics }
    }

    fn graphics_near(&self, doc: &LayoutDocument, block: usize) -> [bool; 2] {
        let Some(block) = doc.blocks.get(block) else {
            return [false; 2];
        };
        let mut flags = [false; 2];
        if let Some(graphics) = self.graphics.get(&block.page) {
            for graphic in graphics {
                // Document.getConnectedGraphics compares vertical edge distances.
                if (graphic.bbox.y2() - block.bbox.y).abs() < 100.0
                    || (graphic.bbox.y - block.bbox.y2()).abs() < 100.0
                {
                    flags[usize::from(graphic.kind != GraphicKind::Image)] = true;
                    if flags == [true, true] {
                        break;
                    }
                }
            }
        }
        flags
    }
}

fn punctuation_profile(text: &str) -> String {
    text.chars()
        .filter(|&ch| !ch.is_whitespace() && tokenizer::FULL_PUNCTUATIONS.contains(ch))
        .collect()
}

fn pattern(text: &str) -> String {
    text.bytes()
        .filter(u8::is_ascii_alphabetic)
        .map(|b| b.to_ascii_lowercase() as char)
        .collect()
}

fn line_text(tokens: &[LayoutToken]) -> String {
    let mut result = String::new();
    for token in tokens {
        result.push_str(&token.text);
        if has_layout_space(token) {
            result.push(' ');
        }
    }
    result
}

fn is_block_last(doc: &LayoutDocument, index: usize, token: &LayoutToken) -> bool {
    doc.blocks
        .get(token.block_idx)
        .is_some_and(|block| block.token_range.end == index + 1)
}

fn block_grobid_token_count(tokens: &[LayoutToken]) -> usize {
    tokens
        .iter()
        .map(|token| 1 + usize::from(has_layout_space(token)) + usize::from(token.new_line_after))
        .sum::<usize>()
        + usize::from(!tokens.is_empty())
}

pub fn extract_segmentation_features(
    doc: &LayoutDocument,
    lexicon: &Lexicon,
) -> (FeatureMatrix, Vec<usize>) {
    let input = extract_segmentation_input(doc, lexicon);
    (input.features, input.line_indices)
}

pub fn extract_segmentation_input(doc: &LayoutDocument, lexicon: &Lexicon) -> SegmentationInput {
    let geometry = Geometry::new(doc);
    let texts: Vec<_> = (0..doc.lines.len())
        .map(|i| line_text(doc.line_tokens(i)))
        .collect();
    let block_counts: Vec<_> = (0..doc.blocks.len())
        .map(|i| block_grobid_token_count(doc.block_tokens(i)))
        .collect();
    let total: usize = block_counts.iter().sum();
    let mut page_totals = HashMap::<_, usize>::new();
    let mut patterns = HashMap::<String, usize>::new();
    let near_page_edge = |index, page_number| {
        geometry.pages.get(&page_number).is_some_and(|page| {
            index < page.block_range.start + 2 || index + 1 == page.block_range.end
        })
    };

    for (i, block) in doc.blocks.iter().enumerate() {
        *page_totals.entry(block.page).or_default() += block_counts[i];
        if near_page_edge(i, block.page)
            && let Some(line) = texts.get(block.line_range.start)
        {
            let pattern = pattern(line);
            if pattern.len() > 8 {
                *patterns.entry(pattern).or_default() += 1;
            }
        }
    }

    let mut seen = HashSet::new();
    let mut document_position = 0;
    let mut page_positions = HashMap::<_, usize>::new();
    let mut font_state = FontState::default();
    let mut output = SegmentationInput {
        features: FeatureMatrix {
            rows: Vec::with_capacity(doc.lines.len()),
        },
        line_indices: Vec::with_capacity(doc.lines.len()),
    };
    let mut previous_page = None;
    for (i, block) in doc.blocks.iter().enumerate() {
        let page_position = page_positions.entry(block.page).or_default();
        let longest = texts
            .get(block.line_range.clone())
            .unwrap_or_default()
            .iter()
            .map(|text| utf16_len(text))
            .max()
            .unwrap_or(0);
        let graphics = geometry.graphics_near(doc, i);
        let in_main_area = geometry.pages.get(&block.page).is_some_and(|page| {
            page.main_area.intersects(&block.bbox)
                || (page.main_area.contains_point(block.bbox.x, block.bbox.y)
                    && page
                        .main_area
                        .contains_point(block.bbox.x2(), block.bbox.y2()))
        });
        if let Some(style_token) = doc.block_tokens(i).first() {
            for line_index in block.line_range.clone() {
                let Some(line) = texts.get(line_index) else {
                    break;
                };
                if filtered(line) {
                    continue;
                }
                let mut terms = line.split_whitespace();
                let Some(first) = terms.next() else { continue };
                let second = terms.next().unwrap_or(first);
                let mut row = Vec::with_capacity(33);
                row.extend([first.into(), second.into(), first.to_lowercase()]);
                row.extend((1..=4).map(|n| prefix(first, n)));
                let block_start = line_index == block.line_range.start
                    || output
                        .features
                        .rows
                        .last()
                        .is_some_and(|row| row[7] == "BLOCKEND");
                let new_page = previous_page != Some(block.page);
                if new_page && let Some(previous) = output.features.rows.last_mut() {
                    previous[8] = PAGE[2].into();
                }
                previous_page = Some(block.page);
                columns(
                    &mut row,
                    [
                        boundary(block_start, line_index + 1 == block.line_range.end, BLOCK),
                        boundary(new_page, false, PAGE),
                    ],
                );
                columns(&mut row, font_state.columns(style_token));
                columns(&mut row, token_classes(first, false));
                columns(
                    &mut row,
                    [
                        flag(is_proper_name(first, lexicon)),
                        flag(lexicon.in_dictionary(first)),
                        "0",
                        flag(is_year(first)),
                        flag(is_month(first)),
                        flag(is_email(first)),
                        flag(is_http(first)),
                    ],
                );
                row.push(relative_document_position(document_position, total).to_string());
                row.push(
                    relative_document_position(*page_position, page_totals[&block.page])
                        .to_string(),
                );
                let profile = punctuation_profile(line);
                let profile_length = utf16_len(&profile);
                row.push(if profile.is_empty() {
                    "no".into()
                } else {
                    profile
                });
                row.push(profile_length.to_string());
                row.push(linear_scaling(utf16_len(line), longest, 10).to_string());
                let repeated = near_page_edge(i, block.page)
                    .then(|| pattern(line))
                    .filter(|pattern| patterns.get(pattern).is_some_and(|count| *count > 1));
                columns(
                    &mut row,
                    [
                        flag(graphics[0]),
                        flag(graphics[1]),
                        flag(repeated.is_some()),
                        flag(repeated.is_some_and(|pattern| seen.insert(pattern))),
                        flag(in_main_area),
                    ],
                );
                output.features.rows.push(row);
                output.line_indices.push(line_index);
            }
        }
        document_position += block_counts[i];
        *page_position += block_counts[i];
    }
    output
}

struct BlockLexicon {
    token_start: usize,
    locations: Vec<bool>,
    emails: Vec<bool>,
    urls: Vec<bool>,
}

impl BlockLexicon {
    fn new(doc: &LayoutDocument, block: usize, lexicon: &Lexicon) -> Self {
        let tokens = doc.block_tokens(block);
        let parts = layout_parts(tokens);
        let locations = match_flags(parts.len(), lexicon.in_location_positions(&parts));
        let emails = address_flags(&parts, true);
        let urls = address_flags(&parts, false);
        let project = |flags: Vec<bool>| {
            let mut i = 0;
            tokens
                .iter()
                .map(|token| {
                    let result = flags[i];
                    i += 1
                        + usize::from(has_layout_space(token))
                        + usize::from(token.new_line_after);
                    result
                })
                .collect()
        };
        Self {
            token_start: doc
                .blocks
                .get(block)
                .map_or(0, |block| block.token_range.start),
            locations: project(locations),
            emails: project(emails),
            urls: project(urls),
        }
    }

    fn flags(&self, index: usize) -> [bool; 3] {
        let i = index.saturating_sub(self.token_start);
        [&self.locations, &self.emails, &self.urls]
            .map(|flags| flags.get(i).copied().unwrap_or(false))
    }
}

fn font_statistics(doc: &LayoutDocument, indices: &[usize]) -> (f64, f64, f64) {
    let mut smallest = 100000.0_f64;
    let mut largest = 0.0_f64;
    let mut sum = 0.0;
    let mut count = 0;
    let mut seen = HashSet::new();
    for &i in indices {
        let Some(token) = doc.tokens.get(i) else {
            continue;
        };
        if !seen.insert(token.block_idx) {
            continue;
        }
        let block = doc.block_tokens(token.block_idx);
        let block = if block.is_empty() {
            std::slice::from_ref(token)
        } else {
            block
        };
        let raw_count = block_grobid_token_count(block);
        if raw_count > block.len() {
            // pdfalto gives synthetic whitespace tokens a zero font size.
            smallest = 0.0;
        }
        count += raw_count;
        for token in block {
            smallest = smallest.min(token.font_size);
            largest = largest.max(token.font_size);
            sum += token.font_size;
        }
    }
    (smallest, largest, sum / count as f64)
}

struct SelectedToken<'a> {
    index: usize,
    token: &'a LayoutToken,
    text: std::borrow::Cow<'a, str>,
}

fn selected_tokens<'a>(doc: &'a LayoutDocument, indices: &[usize]) -> Vec<SelectedToken<'a>> {
    indices
        .iter()
        .filter_map(|&index| {
            let token = doc.tokens.get(index)?;
            Some(SelectedToken {
                index,
                token,
                text: clean_token(&token.text)?,
            })
        })
        .collect()
}

pub fn extract_header_features(
    doc: &LayoutDocument,
    token_indices: &[usize],
    lexicon: &Lexicon,
) -> FeatureMatrix {
    let tokens = selected_tokens(doc, token_indices);
    let (smallest, largest, average) = font_statistics(doc, token_indices);
    let mut alignment = Alignment::default();
    let mut font = FontState::default();
    let mut block_lexicon = None;
    let mut rows = Vec::with_capacity(tokens.len());
    for (i, current) in tokens.iter().enumerate() {
        let SelectedToken { index, token, text } = current;
        let previous = i.checked_sub(1).map(|i| &tokens[i]);
        let next = tokens.get(i + 1);
        let block_start =
            previous.is_none_or(|previous| previous.token.block_idx != token.block_idx);
        let block_end = next.is_none_or(|next| next.token.block_idx != token.block_idx);
        let line_start = block_start
            || previous.is_some_and(|prev| {
                prev.token.line_idx != token.line_idx || prev.token.new_line_after
            });
        let line_end = block_end
            || next.is_some_and(|next| next.token.line_idx != token.line_idx)
            || token.new_line_after;
        if block_start {
            block_lexicon = Some(BlockLexicon::new(doc, token.block_idx, lexicon));
            // HeaderParser flushes the preceding block before updating indentation.
            alignment.x = None;
        }
        if line_start && !block_start {
            alignment.update(token, text);
        }
        let [location, email, url] = block_lexicon
            .as_ref()
            .map_or([false; 3], |matches| matches.flags(*index));
        let mut row = lexical_columns(text, 32);
        columns(
            &mut row,
            [
                if block_end {
                    BLOCK[2]
                } else {
                    boundary(block_start, false, BLOCK)
                },
                if block_end {
                    LINE[2]
                } else {
                    boundary(line_start, line_end, LINE)
                },
                alignment.label(),
            ],
        );
        columns(&mut row, font.columns(token));
        columns(&mut row, token_classes(text, false));
        columns(
            &mut row,
            [
                flag(is_proper_name(text, lexicon)),
                flag(lexicon.in_dictionary(text)),
                flag(is_year(text)),
                flag(is_month(text)),
                flag(location),
                flag(email),
                flag(url),
                punct_type(text),
                flag(token.font_size == largest),
                flag(token.font_size == smallest),
                flag(token.font_size > average),
                "0",
            ],
        );
        rows.push(row);
    }
    FeatureMatrix { rows }
}

static NUMBERED_CALLOUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\[(]?[\t\n\x0b\x0c\r ]*(?:[0-9]+[-−–][0-9]+,|[0-9]+, *)* *(?:[0-9]+[-–][0-9]+|[0-9]+)[\t\n\x0b\x0c\r ]*[)\]]?")
        .expect("valid GROBID numbered callout pattern")
});
static YEAR_CALLOUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[12][0-9]{3}[a-d]?").expect("valid GROBID callout year pattern"));
static AUTHOR_CALLOUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Z]\p{L}+").expect("valid GROBID callout author pattern"));

struct Callouts<'a> {
    kind: &'static str,
    known: HashSet<&'a str>,
}

impl<'a> Callouts<'a> {
    fn new(references: &[ReferenceCallout<'a>]) -> Self {
        let mut numbered = 0;
        let mut authors = 0;
        let mut known = HashSet::new();
        for reference in references {
            if let Some(label) = reference.label {
                known.insert(label);
                let label = label.trim();
                let has_author = AUTHOR_CALLOUT.is_match(label);
                if NUMBERED_CALLOUT
                    .find_iter(label)
                    .any(|hit| has_author || (hit.start() == 0 && hit.end() == label.len()))
                {
                    numbered += 1;
                } else if has_author && YEAR_CALLOUT.is_match(label) {
                    authors += 1;
                }
            }
            if let Some(author) = reference.first_author {
                known.insert(author);
            }
        }
        let kind = if numbered > references.len() / 2 {
            "NUMBER"
        } else if authors > references.len() / 2 {
            "AUTHOR"
        } else {
            "UNKNOWN"
        };
        Self { kind, known }
    }
}

pub fn extract_fulltext_features(
    doc: &LayoutDocument,
    token_indices: &[usize],
    _lexicon: &Lexicon,
) -> FeatureMatrix {
    extract_fulltext_input(doc, token_indices, &[]).features
}

/// Generates body rows with the bibliography context used by FullTextParser.
pub fn extract_fulltext_input(
    doc: &LayoutDocument,
    token_indices: &[usize],
    references: &[ReferenceCallout<'_>],
) -> FulltextInput {
    fulltext_rows(doc, token_indices, references, None)
}

fn selected_char_positions(doc: &LayoutDocument, indices: &[usize]) -> (Vec<usize>, usize) {
    let mut total = 0;
    let mut offsets = Vec::with_capacity(indices.len());
    for (i, &index) in indices.iter().enumerate() {
        offsets.push(total);
        let Some(token) = doc.tokens.get(index) else {
            continue;
        };
        total += utf16_len(&token.text) + usize::from(has_layout_space(token));
        if indices.get(i + 1) == Some(&(index + 1)) {
            total +=
                usize::from(token.new_line_after) + usize::from(is_block_last(doc, index, token));
        }
    }
    (offsets, total)
}

fn fulltext_rows(
    doc: &LayoutDocument,
    indices: &[usize],
    references: &[ReferenceCallout<'_>],
    wanted: Option<&HashSet<usize>>,
) -> FulltextInput {
    let geometry = Geometry::new(doc);
    let tokens = selected_tokens(doc, indices);
    let (offsets, total) = selected_char_positions(doc, indices);
    let callouts = Callouts::new(references);
    let mut alignment = Alignment::default();
    let mut font = FontState::default();
    let mut graphics = [false; 2];
    let mut offset_index = 0;
    let capacity = wanted.map_or(tokens.len(), HashSet::len);
    let mut output = FulltextInput {
        features: FeatureMatrix {
            rows: Vec::with_capacity(capacity),
        },
        token_indices: Vec::with_capacity(capacity),
    };

    for (i, current) in tokens.iter().enumerate() {
        let SelectedToken { index, token, text } = current;
        while indices[offset_index] != *index {
            offset_index += 1;
        }
        let position = offsets[offset_index];
        offset_index += 1;

        let prev = i.checked_sub(1).map(|i| &tokens[i]);
        let next = tokens.get(i + 1);
        let block_start = prev.is_none_or(|prev| prev.token.block_idx != token.block_idx);
        let block_end = next.is_none_or(|next| next.token.block_idx != token.block_idx);
        let actual_block_end = is_block_last(doc, *index, token);
        let line_start = block_start
            || prev.is_some_and(|prev| {
                prev.token.line_idx != token.line_idx || prev.token.new_line_after
            });
        let line_end = token.new_line_after
            || next.is_some_and(|next| next.token.line_idx != token.line_idx || block_end);
        if line_start {
            alignment.update(token, text);
        }
        if block_start {
            graphics = geometry.graphics_near(doc, token.block_idx);
        }
        let style = font.columns(token);
        if wanted.is_some_and(|wanted| !wanted.contains(index)) {
            continue;
        }
        let mut row = lexical_columns(text, 27);
        let end_block =
            (block_end && next.is_some()) || (actual_block_end && !token.new_line_after);
        columns(
            &mut row,
            [
                boundary(block_start, end_block, BLOCK),
                if end_block && !block_start {
                    LINE[2]
                } else {
                    boundary(line_start, line_end, LINE)
                },
                alignment.label(),
            ],
        );
        columns(&mut row, style);
        columns(&mut row, token_classes(text, false));
        row.push(punct_type(text).into());
        row.push(relative_document_position(position, total).to_string());
        let height = geometry
            .pages
            .get(&token.page)
            .map_or(0.0, |page| page.height);
        row.push(discretize_position(token.bbox.y, height, 12).to_string());
        columns(
            &mut row,
            [
                flag(graphics[0]),
                callouts.kind,
                flag(callouts.known.contains(text.as_ref())),
                flag(token.superscript),
            ],
        );
        output.features.rows.push(row);
        output.token_indices.push(*index);
    }
    output
}

/// Uses the document as fulltext context. When a body section and bibliography
/// are already available, use `extract_fulltext_input` and `FulltextInput::select`.
pub fn extract_figure_features(
    doc: &LayoutDocument,
    token_indices: &[usize],
    _lexicon: &Lexicon,
) -> FeatureMatrix {
    caption_rows(doc, token_indices)
}

/// TableParser uses the same fulltext columns as FigureParser.
pub fn extract_table_features(
    doc: &LayoutDocument,
    token_indices: &[usize],
    _lexicon: &Lexicon,
) -> FeatureMatrix {
    caption_rows(doc, token_indices)
}

fn caption_rows(doc: &LayoutDocument, token_indices: &[usize]) -> FeatureMatrix {
    if token_indices.is_empty() {
        return FeatureMatrix::default();
    }
    let requested: HashSet<_> = token_indices.iter().copied().collect();
    let all: Vec<_> = (0..doc.tokens.len()).collect();
    fulltext_rows(doc, &all, &[], Some(&requested)).select(token_indices)
}

#[derive(Clone, Copy)]
struct ReferenceLine {
    length: usize,
    punctuation_count: usize,
}

impl ReferenceLine {
    fn new(text: &str) -> Self {
        Self {
            length: utf16_len(text),
            punctuation_count: text
                .chars()
                .filter(|&ch| ch != ' ' && tokenizer::FULL_PUNCTUATIONS.contains(ch))
                .count(),
        }
    }
}

struct ReferencePosition {
    line_status: &'static str,
    block_status: &'static str,
    alignment: &'static str,
    offset: usize,
    max_line_length: usize,
    line: ReferenceLine,
}

fn reference_row(text: &str, positions: ReferencePosition, lexicon: &Lexicon) -> Vec<String> {
    let mut row = lexical_columns(text, 28);
    columns(&mut row, [positions.line_status, positions.alignment]);
    columns(&mut row, token_classes(text, false));
    columns(
        &mut row,
        [
            flag(is_proper_name(text, lexicon)),
            flag(lexicon.in_dictionary(text)),
            "0",
            "0",
            flag(is_year(text)),
            flag(is_month(text)),
            flag(is_http(text)),
            punct_type(text),
        ],
    );
    row.push(linear_scaling(positions.offset, positions.line.length, 10).to_string());
    row.push(linear_scaling(positions.line.length, positions.max_line_length, 10).to_string());
    row.push(positions.block_status.into());
    row.push(match positions.line.punctuation_count {
        0 => "no".into(),
        count => count.min(10).to_string(),
    });
    row.push("0".into());
    row
}

pub fn extract_reference_segmenter_features(
    doc: &LayoutDocument,
    token_indices: &[usize],
    lexicon: &Lexicon,
) -> FeatureMatrix {
    extract_reference_segmenter_input(doc, token_indices, lexicon).features
}

pub fn extract_reference_segmenter_input(
    doc: &LayoutDocument,
    token_indices: &[usize],
    lexicon: &Lexicon,
) -> RefSegmenterInput {
    let tokens = selected_tokens(doc, token_indices);
    let range = |token: &SelectedToken<'_>| {
        doc.lines
            .get(token.token.line_idx)
            .map_or(token.index..token.index + 1, |line| {
                token.index
                    ..line
                        .token_range
                        .end
                        .min(doc.tokens.len())
                        .max(token.index + 1)
            })
    };
    let mut max_line_length = 1;
    let mut current_length = 0;
    for (i, current) in tokens.iter().enumerate() {
        current_length +=
            utf16_len(&current.token.text) + usize::from(has_layout_space(current.token));
        if current.token.new_line_after {
            if tokens
                .get(i + 1)
                .is_some_and(|next| next.index == current.index + 1)
            {
                max_line_length = max_line_length.max(current_length + 1);
            }
            current_length = 0;
        }
    }

    let mut output = RefSegmenterInput {
        features: FeatureMatrix {
            rows: Vec::with_capacity(tokens.len()),
        },
        line_indices: Vec::with_capacity(tokens.len()),
        token_indices: Vec::with_capacity(tokens.len()),
    };
    let mut alignment = Alignment::default();
    let mut line = ReferenceLine::new("");
    let mut offset = 0;
    for (i, token) in tokens.iter().enumerate() {
        let previous = i.checked_sub(1).map(|i| &tokens[i]);
        let block_start = doc
            .blocks
            .get(token.token.block_idx)
            .is_some_and(|block| block.token_range.start == token.index);
        let line_start = previous.is_none_or(|prev| {
            prev.token.line_idx != token.token.line_idx || prev.token.new_line_after
        });
        let line_end = token.token.new_line_after || range(token).len() == 1;
        if line_start {
            if i > 0 {
                alignment.update(token.token, &token.text);
            }
            let line_tokens = &doc.tokens[range(token)];
            let mut text = line_text(line_tokens);
            if line_tokens.last().is_some_and(|last| last.new_line_after) {
                text.push('\n');
            }
            line = ReferenceLine::new(&text);
            offset = 0;
        } else if previous.is_some_and(|prev| has_layout_space(prev.token)) {
            // ReferenceSegmenterParser increments nn once for the character
            // and once again for a space token.
            offset += 2;
        }
        offset += utf16_len(&token.text);
        if token.index == 0 {
            offset = 0;
        }
        let block_end = is_block_last(doc, token.index, token.token);
        let row = reference_row(
            &token.text,
            ReferencePosition {
                line_status: boundary(line_start, line_end, LINE),
                block_status: boundary(block_start || token.index == 0, block_end, BLOCK),
                alignment: alignment.label(),
                offset,
                max_line_length,
                line,
            },
            lexicon,
        );
        output.features.rows.push(row);
        output.line_indices.push(token.token.line_idx);
        output.token_indices.push(token.index);
    }
    output
}

/// Reference segmentation on analyzer tokens, using left-aligned lines in a
/// single synthetic block when PDF geometry is unavailable.
pub fn extract_reference_segmenter_features_text(text: &str, lexicon: &Lexicon) -> FeatureMatrix {
    let parts = tokenizer::tokenize_with_spaces(text);
    let lines: Vec<_> = parts.split_inclusive(|part| part == "\n").collect();
    let max_line_length = lines
        .iter()
        .filter(|line| line.last().is_some_and(|part| part == "\n"))
        .map(|line| line.iter().map(|part| utf16_len(part)).sum::<usize>())
        .max()
        .unwrap_or(1);
    let mut rows = Vec::new();
    for (line_index, parts) in lines.iter().enumerate() {
        let stats = ReferenceLine::new(&parts.concat());
        let last = parts.iter().rposition(|part| part != " " && part != "\n");
        let mut offset = 0;
        let mut line_start = true;
        for (i, part) in parts.iter().enumerate() {
            if part == " " {
                offset += 2;
                continue;
            }
            let Some(text) = clean_token(part) else {
                continue;
            };
            let block_start = rows.is_empty();
            offset = if block_start {
                0
            } else {
                offset + utf16_len(&text)
            };
            rows.push(reference_row(
                &text,
                ReferencePosition {
                    line_status: boundary(line_start, last == Some(i), LINE),
                    block_status: boundary(
                        block_start,
                        last == Some(i) && line_index + 1 == lines.len(),
                        BLOCK,
                    ),
                    alignment: "ALIGNEDLEFT",
                    offset,
                    max_line_length,
                    line: stats,
                },
                lexicon,
            ));
            line_start = false;
        }
    }
    FeatureMatrix { rows }
}
