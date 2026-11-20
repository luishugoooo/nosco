//! Lexical predicates and numeric transformations from GROBID's FeatureFactory.

use std::sync::LazyLock;

use nosco_lexicon::Lexicon;
use regex::Regex;

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^[a-zA-Z0-9_'^&amp;/+-]+(?:\.[a-zA-Z0-9_'^&amp;/+-]+)*@",
        r"(?:(?:\[?(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}",
        r"(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\]?|",
        r"(?:[a-zA-Z0-9-]+\.)+[a-zA-Z]{2,}\.?)$",
    ))
    .expect("valid FeatureFactory email pattern")
});
static DECIMAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{Nd}$").expect("valid decimal character class"));
static LETTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{L}$").expect("valid letter character class"));

fn java_digit(ch: char) -> bool {
    ch.is_ascii_digit()
        || (!ch.is_ascii() && ch.len_utf16() == 1 && DECIMAL.is_match(ch.encode_utf8(&mut [0; 4])))
}

fn java_upper(ch: char) -> bool {
    ch.len_utf16() == 1 && ch.is_uppercase()
}

pub fn capitalization(s: &str) -> &'static str {
    if s.is_empty() {
        "NOCAPS"
    } else if !s.chars().any(|c| c.len_utf16() == 1 && c.is_lowercase()) {
        "ALLCAPS"
    } else if s.starts_with(java_upper) {
        "INITCAP"
    } else {
        "NOCAPS"
    }
}

pub fn digit_class(s: &str) -> &'static str {
    if !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) {
        "ALLDIGIT"
    } else if s.chars().any(java_digit) {
        "CONTAINSDIGITS"
    } else {
        "NODIGIT"
    }
}

pub(crate) fn affiliation_digit_class(s: &str) -> &'static str {
    if !s.is_empty() && s.chars().all(java_digit) {
        "ALLDIGIT"
    } else {
        digit_class(s)
    }
}

pub fn is_single_char(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|ch| ch.len_utf16() == 1 && ch.len_utf8() == s.len())
}

/// Uses the same substring match as Java's `[1,2][0-9][0-9][0-9]`.
pub fn is_year(s: &str) -> bool {
    s.as_bytes()
        .windows(4)
        .any(|w| matches!(w[0], b'1' | b',' | b'2') && w[1..].iter().all(u8::is_ascii_digit))
}

pub fn is_month(s: &str) -> bool {
    const MONTHS: [&str; 23] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
        "jan",
        "feb",
        "mar",
        "apr",
        "jun",
        "jul",
        "aug",
        "sep",
        "oct",
        "nov",
        "dec",
    ];
    MONTHS.iter().any(|m| s.eq_ignore_ascii_case(m))
}

/// A numeric calendar day. The upstream date row has year and month flags.
pub fn is_day(s: &str) -> bool {
    (1..=2).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_digit())
        && s.parse::<u8>().is_ok_and(|n| (1..=31).contains(&n))
}

pub fn is_http(s: &str) -> bool {
    s.contains("http")
}

pub fn is_email(s: &str) -> bool {
    s.contains('@') && EMAIL.is_match(s)
}

pub fn is_proper_name(s: &str, lexicon: &Lexicon) -> bool {
    lexicon.in_first_names(s) || lexicon.in_last_names(s)
}

pub fn punct_type(s: &str) -> &'static str {
    match s {
        "(" | "[" => "OPENBRACKET",
        ")" | "]" => "ENDBRACKET",
        "." => "DOT",
        "," => "COMMA",
        "-" => "HYPHEN",
        "\"" | "'" | "`" => "QUOTE",
        _ if !s.is_empty() && s.bytes().all(|b| b",:;?.".contains(&b)) => "PUNCT",
        _ => "NOPUNCT",
    }
}

pub fn prefix(s: &str, n: usize) -> String {
    s[..s.char_indices().nth(n).map_or(s.len(), |(i, _)| i)].to_owned()
}

pub fn suffix(s: &str, n: usize) -> String {
    let start = if n == 0 {
        s.len()
    } else {
        s.char_indices().rev().nth(n - 1).map_or(0, |(i, _)| i)
    };
    s[start..].to_owned()
}

pub(crate) fn java_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
        || (!c.is_ascii() && c.len_utf16() == 1 && LETTER.is_match(c.encode_utf8(&mut [0; 4])))
}

fn shape_char(c: char) -> char {
    if java_letter(c) {
        if java_upper(c) { 'X' } else { 'x' }
    } else if java_digit(c) {
        'd'
    } else {
        c
    }
}

/// Retains the leading shape, a compressed middle, and the two final shapes.
pub fn word_shape(s: &str) -> String {
    let shape: Vec<_> = s.chars().map(shape_char).collect();
    if shape.len() <= 3 {
        return shape.into_iter().collect();
    }

    let mut output = String::with_capacity(s.len());
    output.push(shape[0]);
    let limit = shape.len() - 2;
    let mut i = 1;
    let mut middle = shape[1];
    while i < limit {
        output.push(middle);
        while i < limit && shape[i] == middle {
            i += 1;
        }
        middle = shape[i];
        // TextUtilities uses a for loop, which advances after skipping a run.
        i += 1;
    }
    if !output.ends_with(middle) {
        output.push(middle);
    }
    output.extend(&shape[limit..]);
    output
}

pub fn word_shape_trimmed(s: &str) -> String {
    let shape: Vec<_> = s.chars().map(shape_char).collect();
    let Some(&first) = shape.first() else {
        return String::new();
    };
    let mut output = String::with_capacity(s.len());
    let mut i = 0;
    let mut current = first;
    while i < shape.len() {
        output.push(current);
        while i + 1 < shape.len() && shape[i] == current {
            i += 1;
        }
        current = shape[i];
        i += 1;
    }
    if !output.ends_with(current) {
        output.push(current);
    }
    output
}

pub fn linear_scaling(pos: usize, total: usize, nb_bins: usize) -> usize {
    if pos >= total {
        nb_bins
    } else {
        (pos as f32 / total as f32 * nb_bins as f32) as usize
    }
}

pub fn relative_document_position(pos: usize, total: usize) -> usize {
    linear_scaling(pos, total, 12)
}

pub fn discretize_position(pos: f64, total: f64, nb_bins: usize) -> usize {
    if pos >= total {
        nb_bins
    } else if pos <= 0.0 {
        0
    } else {
        (pos / total * nb_bins as f64) as usize
    }
}

pub fn shadow_numbers(s: &str) -> String {
    s.chars()
        .map(|ch| if java_digit(ch) { 'X' } else { ch })
        .collect()
}

pub(crate) fn utf16_len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.encode_utf16().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capitalization_and_digit_classes() {
        for (input, caps, digits) in [
            ("", "NOCAPS", "NODIGIT"),
            ("ÉTUDE", "ALLCAPS", "NODIGIT"),
            ("Émile", "INITCAP", "NODIGIT"),
            ("éMile", "NOCAPS", "NODIGIT"),
            ("?", "ALLCAPS", "NODIGIT"),
            ("42", "ALLCAPS", "ALLDIGIT"),
            ("A4", "ALLCAPS", "CONTAINSDIGITS"),
            ("١٢", "ALLCAPS", "CONTAINSDIGITS"),
            ("²", "ALLCAPS", "NODIGIT"),
        ] {
            assert_eq!(capitalization(input), caps);
            assert_eq!(digit_class(input), digits);
        }
        assert_eq!(affiliation_digit_class("١٢"), "ALLDIGIT");
        assert!(is_single_char("é"));
        assert!(!is_single_char("𐐀"));
        assert!(!is_single_char(""));
    }

    #[test]
    fn date_and_address_patterns() {
        for text in ["1999", "x2025a", ",123", "20199"] {
            assert!(is_year(text), "{text}");
        }
        for text in ["999", "9999", "２０２５"] {
            assert!(!is_year(text), "{text}");
        }
        for text in ["May", "Sep", "NOVEMBER"] {
            assert!(is_month(text), "{text}");
        }
        assert!(!is_month("Sept"));
        assert!(!is_month("Apr."));
        assert!(is_day("01") && is_day("31"));
        assert!(!is_day("0") && !is_day("32") && !is_day("+4"));
        assert!(is_http("prehttps"));
        assert!(!is_http("HTTP"));
        assert!(is_email("a.b+tag@example.org"));
        assert!(is_email("x@[192.168.1.1]"));
        assert!(!is_email("a b@example.org"));
        assert!(!is_email("a@example"));
        assert!(!is_proper_name("John", &Lexicon::empty()));
    }

    #[test]
    fn punctuation_matches_the_parser_regex_and_overrides() {
        for (token, class) in [
            ("(", "OPENBRACKET"),
            ("]", "ENDBRACKET"),
            (".", "DOT"),
            (",", "COMMA"),
            ("-", "HYPHEN"),
            ("\"", "QUOTE"),
            ("`", "QUOTE"),
            ("...?", "PUNCT"),
            (":", "PUNCT"),
            ("", "NOPUNCT"),
            ("!", "NOPUNCT"),
            ("—", "NOPUNCT"),
            ("“", "NOPUNCT"),
            ("{", "NOPUNCT"),
            ("hey,", "NOPUNCT"),
        ] {
            assert_eq!(punct_type(token), class);
        }
    }

    #[test]
    fn affixes_shapes_and_shadowing() {
        assert_eq!(prefix("éβc", 2), "éβ");
        assert_eq!(suffix("éβc", 2), "βc");
        assert_eq!(prefix("ſ", 4), "ſ");
        assert_eq!(suffix("ſ", 4), "ſ");
        assert_eq!(prefix("a", 0), "");
        assert_eq!(suffix("a", 0), "");
        assert_eq!(word_shape("Mental"), "Xxxx");
        assert_eq!(word_shape("Abc123"), "Xxddd");
        assert_eq!(word_shape("Ab1"), "Xxd");
        assert_eq!(word_shape("a.-b"), "x.--x");
        assert_eq!(word_shape(""), "");
        assert_eq!(word_shape_trimmed("ABabc123---"), "Xxd-");
        assert_eq!(word_shape_trimmed("x"), "x");
        assert_eq!(word_shape_trimmed(""), "");
        assert_eq!(shadow_numbers("G5+١²"), "GX+X²");
    }

    #[test]
    fn positions_follow_java_rounding_and_clamping() {
        assert_eq!(linear_scaling(1, 3, 10), 3);
        assert_eq!(linear_scaling(2, 3, 10), 6);
        assert_eq!(linear_scaling(3, 3, 10), 10);
        assert_eq!(linear_scaling(4, 3, 10), 10);
        assert_eq!(linear_scaling(0, 3, 10), 0);
        assert_eq!(linear_scaling(0, 0, 10), 10);
        assert_eq!(relative_document_position(1, 3), 4);
        assert_eq!(discretize_position(-1.0, 2.0, 10), 0);
        assert_eq!(discretize_position(0.9, 2.0, 10), 4);
        assert_eq!(discretize_position(2.1, 2.0, 10), 10);
    }
}
