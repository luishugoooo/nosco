use std::ops::Range;

use nosco_lexicon::Lexicon;
use nosco_models::BBox;
use nosco_pdf::{BoundingBox, LayoutDocument, LayoutToken};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Span<'a> {
    pub tag: &'a str,
    pub rows: Range<usize>,
}

pub(crate) fn base(label: &str) -> &str {
    label.strip_prefix("I-").unwrap_or(label)
}

/// Wapiti's GROBID labels use I- to start a new span.
pub(crate) fn spans<'a>(labels: &[&'a str]) -> impl Iterator<Item = Span<'a>> {
    let mut end = 0;
    std::iter::from_fn(move || {
        let start = end;
        let tag = base(labels.get(start)?);
        end += 1;
        while labels.get(end).is_some_and(|&label| label == tag) {
            end += 1;
        }
        Some(Span { tag, rows: start..end })
    })
}

pub(crate) fn compact(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn clean(value: &str) -> &str {
    value.trim_matches(|c: char| c.is_whitespace() || ",;:•".contains(c))
}

pub(crate) fn append(field: &mut Option<String>, text: &str) {
    if !text.is_empty() {
        match field {
            Some(value) => {
                value.push(' ');
                value.push_str(text);
            }
            None => *field = Some(text.to_owned()),
        }
    }
}

pub(crate) fn layout_visible(token: &LayoutToken) -> bool {
    !token.text.trim().is_empty() && !token.text.contains("@IMAGE") && !token.text.contains("@PAGE")
}

pub(crate) fn new_line(prev: &LayoutToken, next: &LayoutToken) -> bool {
    prev.new_line_after || prev.line_idx != next.line_idx || prev.page != next.page
}

/// Render selected layout tokens while retaining the input row's byte offsets.
pub(crate) struct Rendered {
    pub text: String,
    pub offsets: Vec<Range<usize>>,
}

fn join_hyphen(left: &str, right: &str, lexicon: &Lexicon) -> bool {
    let left = left.rsplit(|c: char| !c.is_alphabetic()).next().unwrap_or("");
    let right = right.split(|c: char| !c.is_alphabetic()).next().unwrap_or("");
    if left.is_empty() || !right.starts_with(char::is_lowercase) {
        return false;
    }
    let joined = format!("{left}{right}");
    if lexicon.in_dictionary(&joined) {
        return true;
    }
    let left_in = lexicon.in_dictionary(left);
    let right_in = lexicon.in_dictionary(right);
    if !left_in && !right_in {
        return true;
    }
    // Common bound suffixes (e.g. "immunohisto-chemistry", "character-izing", "express-ing")
    // almost always indicate a line-wrap split rather than an intentional compound.
    if !right_in && right.len() >= 2 && left.len() >= 2 {
        return true;
    }
    let left_lower = left.to_ascii_lowercase();
    let known_prefix = matches!(
        left_lower.as_str(),
        "pre" | "non" | "sub" | "co" | "re" | "self" | "well" | "anti" | "multi" | "semi"
            | "inter" | "intra" | "trans" | "cross" | "post" | "over" | "under" | "mid"
            | "bio" | "geo" | "neuro" | "micro" | "macro" | "auto" | "quasi" | "pseudo"
            | "meta" | "hyper" | "super" | "ultra" | "ex" | "pro" | "de" | "bi" | "tri"
            | "un" | "high" | "low" | "long" | "short" | "full" | "half" | "one" | "two"
            | "three" | "four" | "single" | "double" | "triple" | "large" | "small"
    );
    !left_in && left.len() >= 3 && !known_prefix && right.len() >= 3
}

pub(crate) fn render(
    doc: &LayoutDocument,
    indices: &[usize],
    lexicon: &Lexicon,
    keep_lines: bool,
) -> Rendered {
    let mut text = String::new();
    let mut offsets: Vec<Range<usize>> = Vec::with_capacity(indices.len());
    let mut previous = None;
    for &index in indices {
        let token = &doc.tokens[index];
        let part = token.text.trim();
        if let Some(prev_index) = previous {
            let prev = &doc.tokens[prev_index];
            if new_line(prev, token) {
                if !keep_lines
                    && let Some(left) = text.strip_suffix(['-', '\u{ad}', '‐'])
                    && part.starts_with(char::is_alphabetic)
                {
                    if text.ends_with('\u{ad}') || join_hyphen(left, part, lexicon) {
                        text.truncate(left.len());
                        if let Some(offset) = offsets.last_mut() {
                            offset.end = text.len();
                        }
                    }
                } else if !text.ends_with(char::is_whitespace) {
                    text.push(if keep_lines { '\n' } else { ' ' });
                }
            } else if (prev.space_after || index > prev_index + 1)
                && !text.ends_with(char::is_whitespace)
            {
                text.push(' ');
            }
        }
        let start = text.len();
        text.push_str(part);
        offsets.push(start..text.len());
        previous = Some(index);
    }
    Rendered { text, offsets }
}

pub(crate) fn bbox(value: BoundingBox) -> BBox {
    BBox { page: value.page, x: value.x, y: value.y, width: value.width, height: value.height }
}

pub(crate) fn bboxes(doc: &LayoutDocument, indices: &[usize]) -> Vec<BBox> {
    let mut boxes = Vec::new();
    let mut line = None;
    let mut bounds: Option<BoundingBox> = None;
    for &i in indices {
        let token = &doc.tokens[i];
        let key = (token.page, token.line_idx);
        if line != Some(key) && let Some(previous) = bounds.take() {
            boxes.push(bbox(previous));
        }
        bounds = Some(bounds.map_or(token.bbox, |previous| previous.union(&token.bbox)));
        line = Some(key);
    }
    boxes.extend(bounds.map(bbox));
    boxes
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn test_token(text: &str) -> LayoutToken {
        LayoutToken {
            text: text.into(), page: 1, bbox: BoundingBox::new(1, 0.0, 0.0, 0.0, 0.0),
            font_name: String::new(), font_size: 10.0, bold: false, italic: false,
            subscript: false, superscript: false, color: (0, 0, 0), rotated: false,
            space_after: false, new_line_after: false, block_idx: 0, line_idx: 0,
        }
    }

    #[test]
    fn beginning_labels_split_even_for_the_same_tag() {
        let labels = ["<a>", "<a>", "I-<a>", "<a>", "I-<b>", "<a>"];
        let result: Vec<_> = spans(&labels).map(|s| (s.tag, s.rows)).collect();
        assert_eq!(result, [("<a>", 0..2), ("<a>", 2..4), ("<b>", 4..5), ("<a>", 5..6)]);
    }

    #[test]
    fn line_end_hyphens_rejoin_without_inserting_a_space() {
        let doc = LayoutDocument {
            tokens: ["An", "algo", "-", "rithm", "for", "well-", "Known"]
                .into_iter().enumerate().map(|(i, text)| LayoutToken {
                    text: text.into(),
                    space_after: matches!(i, 0 | 3 | 4),
                    new_line_after: matches!(i, 2 | 5),
                    ..test_token(text)
                }).collect(),
            ..Default::default()
        };
        let value = render(&doc, &[0, 1, 2, 3, 4, 5, 6], &Lexicon::empty(), false);
        assert_eq!(value.text, "An algorithm for well-Known");
        assert_eq!(&value.text[value.offsets[3].clone()], "rithm");
    }
}
