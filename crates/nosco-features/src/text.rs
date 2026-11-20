use std::sync::LazyLock;

use nosco_lexicon::Lexicon;
use nosco_pdf::LayoutToken;
use regex::Regex;

use crate::factory::*;
use crate::{
    FeatureMatrix, LINE, boundary, clean_token, columns, flag, lexical_columns, token_classes,
    whitespace_free,
};

static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i-u)(?:(?:https?|ftp)\s{0,2}:\s{0,2}//|www\s{0,2}\.)\s{0,2}",
        r"[-A-Z0-9+&@#/%?=~_()|!:.;]*[-A-Z0-9+&@#/%=~_()]",
    ))
    .expect("valid GROBID URL pattern")
});
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\w+(?:[.\-_,]\w+)?\s?(?:[.\-_,]\w+)?\s?@\s?\w+(?:\s?[.\-]\s?\w+)+)")
        .expect("valid GROBID email pattern")
});
static DOI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"10\.[0-9]{4,5}/[^\x09-\x0d ]+[^;,.\x09-\x0d ]").expect("valid GROBID DOI pattern")
});
static ARXIV: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?-u:arXiv\s?(?:\.org)?\s?:\s?",
        r"(?:[0-9]{4}\s?\.\s?[0-9]{4,5}|[ a-zA-Z.\-]*\s?/\s?[0-9]{7})(?:v[0-9]+)?)",
    ))
    .expect("valid GROBID arXiv pattern")
});

pub(crate) fn match_flags(len: usize, ranges: Vec<(usize, usize)>) -> Vec<bool> {
    let mut result = vec![false; len];
    for (start, end) in ranges {
        result[start..=end].fill(true);
    }
    result
}

/// Project character matches onto token offsets using GROBID's start/end rules.
pub(crate) fn address_flags(tokens: &[&str], email: bool) -> Vec<bool> {
    let pattern = if email { &EMAIL } else { &URL };
    pattern_flags(tokens, &[pattern])
}

fn pattern_flags(tokens: &[&str], patterns: &[&LazyLock<Regex>]) -> Vec<bool> {
    let mut text = String::new();
    let mut starts = Vec::with_capacity(tokens.len());
    for token in tokens {
        starts.push(text.len());
        text.push_str(token);
    }
    let mut flags = vec![false; tokens.len()];
    for pattern in patterns {
        let mut cursor = 0;
        for hit in pattern.find_iter(&text) {
            let start = cursor + starts[cursor..].partition_point(|&offset| offset < hit.start());
            if start == starts.len() {
                break;
            }
            let end = (start + starts[start..].partition_point(|&offset| offset < hit.end()))
                .saturating_sub(1)
                .max(start);
            flags[start..=end].fill(true);
            cursor = end + 1;
        }
    }
    flags
}

/// Takes pre-tokenized date input, with one row per non-whitespace token.
pub fn extract_date_features(tokens: &[&str], _lexicon: &Lexicon) -> FeatureMatrix {
    let tokens: Vec<_> = tokens
        .iter()
        .filter_map(|text| whitespace_free(text))
        .filter(|text| {
            !text.starts_with("@IMAGE")
                && ![".pbm", ".svg", ".jpg", ".png"]
                    .iter()
                    .any(|p| text.contains(p))
        })
        .collect();
    let rows = tokens
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let mut row = lexical_columns(text, 18);
            row.push(boundary(i == 0, i + 1 == tokens.len(), LINE).into());
            columns(&mut row, token_classes(text, false));
            columns(
                &mut row,
                [
                    flag(is_year(text)),
                    flag(is_month(text)),
                    punct_type(text),
                    "<date>",
                ],
            );
            row
        })
        .collect();
    FeatureMatrix { rows }
}

pub fn extract_name_citation_features(tokens: &[&str], lexicon: &Lexicon) -> FeatureMatrix {
    extract_names(tokens, lexicon)
}

pub fn extract_name_citation_features_layout(
    tokens: &[LayoutToken],
    lexicon: &Lexicon,
) -> FeatureMatrix {
    extract_names(&layout_parts(tokens), lexicon)
}

/// The header author parser removes internal line breaks before generating name rows.
pub fn extract_name_header_features(
    tokens: &[&str],
    _line_starts: &[bool],
    lexicon: &Lexicon,
) -> FeatureMatrix {
    let end = tokens.iter().rposition(|token| !token.trim().is_empty());
    let Some(end) = end else {
        return FeatureMatrix::default();
    };
    let tokens: Vec<_> = tokens[..=end]
        .iter()
        .map(|&token| if is_line_break(token) { " " } else { token })
        .collect();
    extract_names(&tokens, lexicon)
}

fn is_line_break(text: &str) -> bool {
    matches!(text, "\n" | "\r" | "\r\n")
}

fn extract_names(tokens: &[&str], lexicon: &Lexicon) -> FeatureMatrix {
    let title = match_flags(tokens.len(), lexicon.in_person_title_positions(tokens));
    let suffix = match_flags(tokens.len(), lexicon.in_person_suffix_positions(tokens));
    let mut rows = Vec::with_capacity(tokens.len());
    for (i, text) in tokens.iter().enumerate() {
        let Some(text) = clean_token(text) else {
            continue;
        };
        let end = i + 1 == tokens.len()
            || tokens[i + 1..]
                .iter()
                .find(|&&t| t != " ")
                .is_some_and(|t| is_line_break(t));
        let mut row = lexical_columns(&text, 21);
        // FeaturesVectorName resets the newline flag at every non-space token.
        row.push(boundary(i == 0, end, LINE).into());
        columns(&mut row, token_classes(&text, false));
        columns(
            &mut row,
            [
                flag(lexicon.in_dictionary(&text)),
                flag(lexicon.in_first_names(&text)),
                flag(lexicon.in_last_names(&text)),
                flag(title[i]),
                flag(suffix[i]),
                punct_type(&text),
                "0",
            ],
        );
        rows.push(row);
    }
    FeatureMatrix { rows }
}

/// Takes one affiliation sequence. Supply line starts and/or explicit newline tokens.
pub fn extract_affiliation_features(
    tokens: &[&str],
    line_starts: &[bool],
    lexicon: &Lexicon,
) -> FeatureMatrix {
    let mut next_starts_line = true;
    let mut visible = Vec::with_capacity(tokens.len());
    for (i, &token) in tokens.iter().enumerate() {
        next_starts_line |= line_starts.get(i).copied().unwrap_or(false) || is_line_break(token);
        if token.contains("@newline") {
            next_starts_line = true;
            continue;
        }
        if let Some(text) = whitespace_free(token) {
            visible.push((text, next_starts_line));
            next_starts_line = false;
        }
    }
    let terms: Vec<_> = visible.iter().map(|(text, _)| text.as_ref()).collect();
    let location = match_flags(terms.len(), lexicon.in_location_positions(&terms));
    let countries = country_flags(&terms, lexicon);
    let rows = visible
        .iter()
        .enumerate()
        .map(|(i, (text, start))| {
            let end = visible.get(i + 1).is_none_or(|(_, start)| *start);
            let mut row = lexical_columns(text, 22);
            row.push(boundary(*start, end, LINE).into());
            columns(&mut row, token_classes(text, true));
            columns(
                &mut row,
                [
                    flag(is_proper_name(text, lexicon)),
                    flag(lexicon.in_dictionary(text)),
                    "0", // The Java affiliation builder never sets firstName.
                    flag(location[i]),
                    flag(countries[i]),
                    punct_type(text),
                ],
            );
            row.push(word_shape(text));
            row.push("<affiliation>".into());
            row
        })
        .collect();
    FeatureMatrix { rows }
}

fn country_flags(terms: &[&str], lexicon: &Lexicon) -> Vec<bool> {
    // The 0.9.1 CountryCodes.xml entries are at most 16 analyzer tokens
    // (excluding whitespace). The existing country map exposes whole names.
    const MAX_COUNTRY_TERMS: usize = 16;
    let mut flags = vec![false; terms.len()];
    let mut phrase = String::new();
    for start in 0..terms.len() {
        phrase.clear();
        for (offset, term) in terms[start..].iter().take(MAX_COUNTRY_TERMS).enumerate() {
            if offset > 0
                && !matches!(*term, "," | "." | ")" | "'" | "-")
                && !phrase.ends_with(['(', '\'', '-'])
            {
                phrase.push(' ');
            }
            phrase.push_str(term);
            if lexicon.is_country(&phrase) {
                flags[start..=start + offset].fill(true);
            }
        }
    }
    flags
}

/// Takes analyzer tokens for one citation. Retain explicit spaces for the
/// gazetteer regexes and Java's sequence-position calculation.
pub fn extract_citation_features(
    tokens: &[&str],
    _line_starts: &[bool],
    lexicon: &Lexicon,
) -> FeatureMatrix {
    let mut parts = Vec::with_capacity(tokens.len());
    for &token in tokens {
        split_subdigits(token, &mut parts);
    }
    let tokens = parts.as_slice();
    let count = tokens.len();
    let journals = match_flags(count, lexicon.in_journal_positions(tokens));
    let abbreviated = match_flags(count, lexicon.in_abbrev_journal_positions(tokens));
    let conferences = match_flags(count, lexicon.in_conference_positions(tokens));
    let publishers = match_flags(count, lexicon.in_publisher_positions(tokens));
    let places = match_flags(count, lexicon.in_location_positions(tokens));
    let collaborations = match_flags(count, lexicon.in_collaboration_positions(tokens));
    let identifiers = pattern_flags(tokens, &[&DOI, &ARXIV]);
    let urls = address_flags(tokens, false);
    let mut rows = Vec::with_capacity(count);
    for (i, text) in tokens.iter().enumerate() {
        let Some(text) = clean_token(text) else {
            continue;
        };
        let mut row = lexical_columns(&text, 30);
        row.push(boundary(i == 0, i + 1 == count, LINE).into());
        columns(&mut row, token_classes(&text, false));
        let first_name = lexicon.in_first_names(&text);
        let last_name = lexicon.in_last_names(&text);
        columns(
            &mut row,
            [
                flag(first_name || last_name),
                flag(lexicon.in_dictionary(&text)),
                flag(first_name),
                flag(last_name),
                flag(places[i]),
                flag(is_year(&text)),
                flag(is_month(&text)),
                flag(urls[i]),
                flag(collaborations[i]),
                flag(journals[i] || abbreviated[i]),
                flag(conferences[i]),
                flag(publishers[i]),
                flag(identifiers[i]),
                punct_type(&text),
            ],
        );
        row.push(linear_scaling(i, count, 12).to_string());
        row.push("0".into());
        rows.push(row);
    }
    FeatureMatrix { rows }
}

fn split_subdigits<'a>(text: &'a str, parts: &mut Vec<&'a str>) {
    let mut start = 0;
    let mut previous = ' ';
    for (offset, current) in text.char_indices() {
        if (previous.is_ascii_digit() && !current.is_ascii_digit())
            || (java_letter(previous) && current.is_ascii_digit())
        {
            parts.push(&text[start..offset]);
            start = offset;
        }
        previous = current;
    }
    parts.push(&text[start..]);
}

pub(crate) fn layout_parts(tokens: &[LayoutToken]) -> Vec<&str> {
    let mut parts = Vec::with_capacity(tokens.len());
    for token in tokens {
        parts.push(token.text.as_str());
        if has_layout_space(token) {
            parts.push(" ");
        }
        if token.new_line_after {
            parts.push("\n");
        }
    }
    parts
}

// The PDF crate stores whitespace as flags. pdfalto also appends a space to
// the final String on each line (except after hyphens), then a newline.
pub(crate) fn has_layout_space(token: &LayoutToken) -> bool {
    token.space_after || (token.new_line_after && !token.text.ends_with('-'))
}
