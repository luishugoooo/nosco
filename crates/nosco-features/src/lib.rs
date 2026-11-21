//! GROBID/Wapiti feature extraction from analyzer tokens and PDF layout.

mod document;
pub mod factory;
mod text;

pub use document::*;
pub use text::*;

use std::borrow::Cow;

/// One observation per row, with columns in the order expected by Wapiti.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeatureMatrix {
    pub rows: Vec<Vec<String>>,
}

impl FeatureMatrix {
    pub fn as_rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn num_cols(&self) -> usize {
        self.rows.first().map_or(0, Vec::len)
    }

    pub fn to_wapiti_lines(&self) -> Vec<String> {
        self.rows.iter().map(|row| row.join(" ")).collect()
    }
}

fn flag(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

fn columns(row: &mut Vec<String>, values: impl IntoIterator<Item = &'static str>) {
    row.extend(values.into_iter().map(str::to_owned));
}

fn lexical_columns(text: &str, width: usize) -> Vec<String> {
    let mut row = Vec::with_capacity(width);
    row.push(text.to_owned());
    row.push(text.to_lowercase());
    row.extend((1..=4).map(|n| factory::prefix(text, n)));
    row.extend((1..=4).map(|n| factory::suffix(text, n)));
    row
}

fn token_classes(text: &str, affiliation: bool) -> [&'static str; 3] {
    let mut digit = if affiliation {
        factory::affiliation_digit_class(text)
    } else {
        factory::digit_class(text)
    };
    let caps = match (digit, factory::capitalization(text), affiliation) {
        ("ALLDIGIT", _, _) => "NOCAPS",
        (_, "ALLCAPS", false) => "ALLCAP",
        (_, caps, _) => caps,
    };
    if affiliation && digit == "CONTAINSDIGITS" {
        digit = "CONTAINDIGIT";
    }
    [caps, digit, flag(factory::is_single_char(text))]
}

fn filtered(text: &str) -> bool {
    text.is_empty() || text.contains("@IMAGE") || text.contains("@PAGE")
}

fn whitespace_free(text: &str) -> Option<Cow<'_, str>> {
    if text.chars().any(char::is_whitespace) {
        let cleaned: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
        (!cleaned.is_empty()).then_some(Cow::Owned(cleaned))
    } else {
        (!text.is_empty()).then_some(Cow::Borrowed(text))
    }
}

fn clean_token(text: &str) -> Option<Cow<'_, str>> {
    whitespace_free(text).filter(|text| !filtered(text))
}

fn boundary(start: bool, end: bool, labels: [&'static str; 3]) -> &'static str {
    labels[if start {
        0
    } else if end {
        2
    } else {
        1
    }]
}

const LINE: [&str; 3] = ["LINESTART", "LINEIN", "LINEEND"];
const BLOCK: [&str; 3] = ["BLOCKSTART", "BLOCKIN", "BLOCKEND"];

/// Whether the PDF token produces a feature row.
pub fn visible_layout_text(text: &str) -> bool {
    clean_token(text).is_some()
}
