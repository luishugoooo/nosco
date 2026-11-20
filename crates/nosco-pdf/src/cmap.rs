use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Code {
    value: u32,
    len: usize,
}

impl Code {
    fn read(bytes: &[u8]) -> Option<Self> {
        (1..=4).contains(&bytes.len()).then(|| Self {
            len: bytes.len(),
            value: bytes
                .iter()
                .fold(0, |value, &byte| (value << 8) | u32::from(byte)),
        })
    }
}

struct CodeRange {
    first: Code,
    end: u32,
}

impl CodeRange {
    fn parse(first: Vec<u8>, last: Vec<u8>) -> Option<Self> {
        let first = Code::read(&first)?;
        let last = Code::read(&last)?;
        (first.len == last.len && first.value <= last.value).then_some(Self {
            first,
            end: last.value,
        })
    }

    fn offset(&self, code: Code) -> Option<u32> {
        (code.len == self.first.len && code.value <= self.end)
            .then(|| code.value.checked_sub(self.first.value))
            .flatten()
    }
}

#[derive(Default)]
pub(crate) struct CMap {
    spaces: Vec<CodeRange>,
    chars: HashMap<Code, (usize, String)>,
    ranges: Vec<(CodeRange, Vec<u8>, usize)>,
    order: usize,
    cids: Vec<(CodeRange, u32)>,
    pub(crate) vertical: bool,
}

impl CMap {
    pub(crate) fn identity(vertical: bool) -> Self {
        let first = Code { value: 0, len: 2 };
        Self {
            spaces: vec![CodeRange { first, end: 65535 }],
            vertical,
            ..Self::default()
        }
    }

    fn insert(&mut self, code: Code, text: String) {
        self.order += 1;
        self.chars.insert(code, (self.order, text));
    }

    pub(crate) fn parse(bytes: &[u8], mut map: Self) -> Self {
        let mut tokens = Lexer(bytes);
        while let Some(token) = tokens.next() {
            match token {
                Token::Word(b"/WMode") => map.vertical = tokens.next() == Some(Token::Word(b"1")),
                Token::Word(b"begincodespacerange") => {
                    while let Some(Token::Hex(first)) = tokens.next() {
                        let Some(Token::Hex(last)) = tokens.next() else {
                            break;
                        };
                        map.spaces.extend(CodeRange::parse(first, last));
                    }
                }
                Token::Word(b"beginbfchar") => {
                    while let Some(Token::Hex(first)) = tokens.next() {
                        let Some(Token::Hex(target)) = tokens.next() else {
                            break;
                        };
                        if let Some(code) = Code::read(&first) {
                            map.insert(code, utf16(&target));
                        }
                    }
                }
                Token::Word(b"beginbfrange") => {
                    while let Some(Token::Hex(first)) = tokens.next() {
                        let Some(Token::Hex(last)) = tokens.next() else {
                            break;
                        };
                        let range = CodeRange::parse(first, last);
                        match tokens.next() {
                            Some(Token::Hex(target)) => {
                                if let Some(range) = range {
                                    // Keep large scalar ranges compact. Small ranges decode once.
                                    if range.end - range.first.value < 16 {
                                        for value in range.first.value..=range.end {
                                            let code = Code {
                                                value,
                                                ..range.first
                                            };
                                            map.insert(
                                                code,
                                                increment(&target, value - range.first.value),
                                            );
                                        }
                                    } else {
                                        map.order += 1;
                                        map.ranges.push((range, target, map.order));
                                    }
                                }
                            }
                            Some(Token::Open) => {
                                let mut offset = 0u32;
                                while let Some(Token::Hex(target)) = tokens.next() {
                                    if let Some(range) = &range
                                        && let Some(value) = range.first.value.checked_add(offset)
                                        && value <= range.end
                                    {
                                        map.insert(
                                            Code {
                                                value,
                                                ..range.first
                                            },
                                            utf16(&target),
                                        );
                                    }
                                    offset = offset.saturating_add(1);
                                }
                            }
                            _ => break,
                        }
                    }
                }
                Token::Word(b"begincidchar") => {
                    while let Some(Token::Hex(first)) = tokens.next() {
                        let Some(cid) = tokens.next().and_then(Token::number) else {
                            break;
                        };
                        if let Some(first) = Code::read(&first) {
                            map.cids.push((
                                CodeRange {
                                    first,
                                    end: first.value,
                                },
                                cid,
                            ));
                        }
                    }
                }
                Token::Word(b"begincidrange") => {
                    while let Some(Token::Hex(first)) = tokens.next() {
                        let Some(Token::Hex(last)) = tokens.next() else {
                            break;
                        };
                        let Some(cid) = tokens.next().and_then(Token::number) else {
                            break;
                        };
                        if let Some(range) = CodeRange::parse(first, last) {
                            map.cids.push((range, cid));
                        }
                    }
                }
                _ => {}
            }
        }
        map
    }

    pub(crate) fn has_unicode(&self) -> bool {
        !self.chars.is_empty() || !self.ranges.is_empty()
    }

    pub(crate) fn code_len(&self, bytes: &[u8]) -> Option<usize> {
        for len in 1..=4.min(bytes.len()) {
            let code = Code::read(&bytes[..len])?;
            let matched = if self.spaces.is_empty() {
                self.chars.contains_key(&code)
                    || self
                        .ranges
                        .iter()
                        .any(|(range, _, _)| range.offset(code).is_some())
                    || self
                        .cids
                        .iter()
                        .any(|(range, _)| range.offset(code).is_some())
            } else {
                self.spaces.iter().any(|range| range.offset(code).is_some())
            };
            if matched {
                return Some(len);
            }
        }
        None
    }

    pub(crate) fn unicode(&self, bytes: &[u8]) -> Option<std::borrow::Cow<'_, str>> {
        let code = Code::read(bytes)?;
        let direct = self.chars.get(&code);
        self.ranges
            .iter()
            .rev()
            .take_while(|(_, _, order)| direct.is_none_or(|(direct, _)| order > direct))
            .find_map(|(range, start, _)| {
                range
                    .offset(code)
                    .map(|offset| increment(start, offset).into())
            })
            .or_else(|| direct.map(|(_, text)| text.as_str().into()))
    }

    pub(crate) fn cid(&self, bytes: &[u8]) -> Option<u32> {
        let code = Code::read(bytes)?;
        self.cids.iter().rev().find_map(|(range, cid)| {
            range
                .offset(code)
                .and_then(|offset| cid.checked_add(offset))
        })
    }
}

fn utf16(bytes: &[u8]) -> String {
    let chars = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes(*pair));
    crate::fonts::normalize(
        char::decode_utf16(chars).map(|ch| ch.unwrap_or(char::REPLACEMENT_CHARACTER)),
    )
}

fn increment(bytes: &[u8], offset: u32) -> String {
    let mut result = bytes.to_vec();
    let mut carry = u64::from(offset);
    for byte in result.iter_mut().rev() {
        carry += u64::from(*byte);
        *byte = carry as u8;
        carry >>= 8;
        if carry == 0 {
            break;
        }
    }
    utf16(&result)
}

#[derive(PartialEq)]
enum Token<'a> {
    Hex(Vec<u8>),
    Word(&'a [u8]),
    Open,
    Other,
}

impl Token<'_> {
    fn number(self) -> Option<u32> {
        match self {
            Self::Word(bytes) => std::str::from_utf8(bytes).ok()?.parse().ok(),
            _ => None,
        }
    }
}

struct Lexer<'a>(&'a [u8]);

impl<'a> Iterator for Lexer<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (&first, rest) = self.0.split_first()?;
            if whitespace(first) {
                self.0 = rest;
            } else if first == b'%' {
                let end = rest
                    .iter()
                    .position(|&byte| matches!(byte, b'\n' | b'\r'))
                    .unwrap_or(rest.len());
                self.0 = &rest[end..];
            } else {
                break;
            }
        }
        let input = self.0;
        let (&first, rest) = input.split_first()?;
        self.0 = rest;
        match first {
            b'<' if rest.first() == Some(&b'<') => {
                self.0 = &rest[1..];
                Some(Token::Other)
            }
            b'<' => {
                let end = rest.iter().position(|&b| b == b'>')?;
                self.0 = &rest[end + 1..];
                let mut bytes = Vec::new();
                let mut high = None;
                for nibble in rest[..end].iter().copied().filter(|&b| !whitespace(b)) {
                    let Some(nibble) = char::from(nibble).to_digit(16) else {
                        return Some(Token::Other);
                    };
                    if let Some(high) = high.take() {
                        bytes.push((high << 4) | nibble as u8);
                    } else {
                        high = Some(nibble as u8);
                    }
                }
                if let Some(high) = high {
                    bytes.push(high << 4);
                }
                Some(Token::Hex(bytes))
            }
            b'(' => {
                let mut depth = 1;
                while let Some((&byte, rest)) = self.0.split_first() {
                    self.0 = rest;
                    match byte {
                        b'\\' if !rest.is_empty() => self.0 = &rest[1..],
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                Some(Token::Other)
            }
            b'[' => Some(Token::Open),
            b'>' | b']' | b')' | b'{' | b'}' => Some(Token::Other),
            _ => {
                let length = 1 + rest
                    .iter()
                    .position(|&b| whitespace(b) || b"<>[]{}()/%".contains(&b))
                    .unwrap_or(rest.len());
                self.0 = &input[length..];
                Some(Token::Word(&input[..length]))
            }
        }
    }
}

fn whitespace(byte: u8) -> bool {
    matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_codespaces_bfchar_and_scalar_and_array_ranges() {
        let cmap = CMap::parse(
            br#"
            % comment containing fake beginbfchar
            /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) >> def
            2 begincodespacerange
            <00> <7f>
            <9000> <9fff>
            endcodespacerange
            2 beginbfchar
            <21> <D83D DE00>
            <9020> <00410301>
            endbfchar
            3 beginbfrange
            <30> <32> <0030>
            <9030> <9032> [<fb01> <0066006c> <00A0>]
            <9100> <9201> <0041>
            endbfrange
        "#,
            CMap::default(),
        );
        assert_eq!(cmap.code_len(&[0x22, 0x91]), Some(1));
        assert_eq!(cmap.code_len(&[0x9f, 0xfc]), Some(2));
        assert_eq!(cmap.code_len(&[0x9f]), None);
        let text = |bytes: &[u8]| cmap.unicode(bytes).unwrap().into_owned();
        assert_eq!(text(b"!"), "😀");
        assert_eq!(text(&[0x90, 0x20]), "A\u{301}");
        assert_eq!(text(b"2"), "2");
        assert_eq!(text(&[0x90, 0x30]), "fi");
        assert_eq!(text(&[0x90, 0x31]), "fl");
        assert_eq!(text(&[0x90, 0x32]), " ");
        assert_eq!(text(&[0x92, 0x00]), "Ł");
    }

    #[test]
    fn child_ranges_override_parent_chars() {
        let base = CMap::parse(
            b"1 beginbfchar <0020> <0058> endbfchar",
            CMap::identity(false),
        );
        let child = CMap::parse(b"1 beginbfrange <0020> <005f> <0061> endbfrange", base);
        assert_eq!(child.code_len(&[0, 32]), Some(2));
        assert_eq!(child.unicode(&[0, 32]).as_deref(), Some("a"));
    }

    #[test]
    fn horizontal_and_vertical_cid_ranges() {
        let cmap = CMap::parse(
            br#"
            /WMode 1 def
            1 begincodespacerange <00> <ff> endcodespacerange
            1 begincidchar <31> 259 endcidchar
            1 begincidrange <40> <4f> 800 endcidrange
        "#,
            CMap::default(),
        );
        assert!(cmap.vertical);
        assert_eq!(cmap.code_len(b"AA"), Some(1));
        assert_eq!(cmap.cid(b"1"), Some(259));
        assert_eq!(cmap.cid(b"O"), Some(815));
        assert_eq!(cmap.cid(b"P"), None);
    }
}
