//! GROBID's default analyzer and punctuation constants.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

macro_rules! full_punctuations {
    () => {
        "(（[ •*,:;?.!/)）-−–‐«»„\"“”‘’'`$#@]*♦♥♣♠\u{a0}。、，・"
    };
}

pub const FULL_PUNCTUATIONS: &str = full_punctuations!();
pub const DELIMITERS: &str = concat!("\n\r\t\x0c\u{a0}\u{200c}", full_punctuations!());
pub const PUNCTUATIONS: &str = " •*,:;?.!)-−–\"“”‘’'`$]*♦♥♣♠\u{a0}。、，・";

static MIDDLE_DOT_WORD_CHAR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\p{L}\p{N}\p{M}--[\p{Han}\p{Hiragana}\p{Katakana}\p{Hangul}]]$")
        .expect("valid Unicode character class")
});
static DASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{Pd}$").expect("valid Unicode character class"));

fn matches_char(regex: &Regex, ch: char) -> bool {
    regex.is_match(ch.encode_utf8(&mut [0; 4]))
}

fn middle_dot_word_char(ch: char) -> bool {
    // The Latin dental click is already a vertical bar at this stage in Java.
    ch.is_ascii_alphanumeric()
        || (!ch.is_ascii() && ch != '\u{01c0}' && matches_char(&MIDDLE_DOT_WORD_CHAR, ch))
}

fn normalize_chars(text: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    let mut chars = text.char_indices().peekable();
    let mut previous = None;
    std::iter::from_fn(move || {
        let (start, ch) = chars.next()?;
        let normalized = match ch {
            '\r' => {
                if chars.peek().is_some_and(|&(_, next)| next == '\n') {
                    chars.next();
                }
                '\n'
            }
            '\n' | '\x0b' | '\x0c' | '\u{85}' => '\n',
            '\t'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{180e}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}' => ' ',
            '\u{203f}'
            | '\u{2040}'
            | '\u{2054}'
            | '\u{fe4d}'..='\u{fe4f}'
            | '\u{ff3f}'
            | '\u{fe33}'
            | '\u{fe34}' => '_',
            '\u{01c0}' | '\u{05c0}' | '\u{2223}' | '\u{2758}' => '|',
            '‣' | '◦' | '⁃' | '⁌' | '⁍' | '∙' | '◘' | '⦾' | '⦿' | '⏺' | '●' | '⚫' | '⬤' => {
                '•'
            }
            '·' if !previous.is_some_and(middle_dot_word_char)
                || !chars
                    .peek()
                    .is_some_and(|&(_, next)| middle_dot_word_char(next)) =>
            {
                '•'
            }
            '（' | '⟮' | '⦅' | '❨' | '❪' | '⟬' => '(',
            '）' | '⟯' | '⦆' | '❩' | '❫' | '⟭' => ')',
            '−' => '-',
            ch if !ch.is_ascii() && matches_char(&DASH, ch) => '-',
            ch => ch,
        };
        previous = Some(normalized);
        Some((start, normalized))
    })
}

pub(crate) fn normalize(text: &str) -> Cow<'_, str> {
    if text.is_ascii() && !text.bytes().any(|b| matches!(b, b'\t' | b'\r' | 11 | 12)) {
        return Cow::Borrowed(text);
    }

    let mut chars = normalize_chars(text);
    for (start, ch) in chars.by_ref() {
        if !text[start..].starts_with(ch) {
            let mut normalized = String::with_capacity(text.len());
            normalized.push_str(&text[..start]);
            normalized.push(ch);
            normalized.extend(chars.map(|(_, ch)| ch));
            return Cow::Owned(normalized);
        }
    }
    Cow::Borrowed(text)
}

const ASCII_DELIMITERS: [bool; 128] = {
    let bytes = DELIMITERS.as_bytes();
    let mut table = [false; 128];
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte.is_ascii() {
            table[byte as usize] = true;
        }
        i += 1;
    }
    table
};

fn is_delimiter(ch: char) -> bool {
    if ch.is_ascii() {
        ASCII_DELIMITERS[ch as usize]
    } else {
        DELIMITERS.contains(ch)
    }
}

fn token_ranges(
    chars: impl Iterator<Item = (usize, char)>,
    text_len: usize,
) -> impl Iterator<Item = (usize, usize, bool)> {
    let mut chars = chars.peekable();
    std::iter::from_fn(move || {
        let (start, ch) = chars.next()?;
        if !is_delimiter(ch) {
            while chars.next_if(|&(_, ch)| !is_delimiter(ch)).is_some() {}
        }
        let end = chars.peek().map_or(text_len, |&(offset, _)| offset);
        Some((start, end, ch == ' ' || ch == '\n'))
    })
}

pub(crate) fn normalized_tokens(text: &str) -> impl Iterator<Item = (&str, bool)> {
    token_ranges(text.char_indices(), text.len())
        .map(|(start, end, space)| (&text[start..end], space))
}

pub(crate) fn is_separator(token: &str) -> bool {
    // Java's matcher uses String.indexOf, so it also skips contiguous delimiter sequences.
    DELIMITERS.contains(token)
}

/// Tokenizes and normalizes text, yielding punctuation and non-whitespace tokens.
pub fn tokenize(text: &str) -> Vec<String> {
    let text = normalize(text);
    normalized_tokens(&text)
        .filter(|&(_, space)| !space)
        .map(|(token, _)| token.to_owned())
        .collect()
}

/// Tokenizes and normalizes text, retaining each space and newline token.
pub fn tokenize_with_spaces(text: &str) -> Vec<String> {
    let text = normalize(text);
    normalized_tokens(&text)
        .map(|(token, _)| token.to_owned())
        .collect()
}

/// Returns non-whitespace token spans as byte ranges (exclusive end) and slices of the input.
/// Boundaries follow the analyzer, and slices retain the original spelling and punctuation.
pub fn tokenize_spans(text: &str) -> Vec<(usize, usize, &str)> {
    token_ranges(normalize_chars(text), text.len())
        .filter(|&(_, _, space)| !space)
        .map(|(start, end, _)| (start, end, &text[start..end]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_delimiters_and_does_not_split_digits() {
        assert_eq!(
            tokenize("Vol. 42b/74; a_b@c"),
            ["Vol", ".", "42b", "/", "74", ";", "a_b", "@", "c"]
        );
        assert_eq!(
            tokenize_with_spaces(" \tA\r\n\x0cB\u{2028}C"),
            [" ", " ", "A", "\n", "\n", "B", " ", "C"]
        );
        assert_eq!(tokenize("a·ǀ"), ["a", "•", "|"]);
        assert!(tokenize("").is_empty());
        assert!(tokenize_spans("\u{180e}\u{a0}").is_empty());
    }

    #[test]
    fn normalizes_punctuation_and_preserves_source_spans() {
        let text = "（é—β）\r\nintel·ligència 威廉·莎士比亚 ◦";
        assert_eq!(
            tokenize(text),
            [
                "(",
                "é",
                "-",
                "β",
                ")",
                "intel·ligència",
                "威廉",
                "•",
                "莎士比亚",
                "•"
            ]
        );
        let spans = tokenize_spans(text);
        assert_eq!(
            spans[..5],
            [
                (0, 3, "（"),
                (3, 5, "é"),
                (5, 8, "—"),
                (8, 10, "β"),
                (10, 13, "）")
            ]
        );
        assert_eq!(spans[5].0, 15);
        assert_eq!(
            spans.iter().map(|span| span.2).collect::<Vec<_>>(),
            [
                "（",
                "é",
                "—",
                "β",
                "）",
                "intel·ligència",
                "威廉",
                "·",
                "莎士比亚",
                "◦"
            ]
        );
    }
}
