use std::ops::Range;

use crate::{CrfError, is_c_space};

/// A compiled Wapiti observation template.
#[derive(Debug)]
pub struct Pattern {
    source: String,
    items: Vec<Item>,
}

#[derive(Debug)]
enum Item {
    Literal(Range<usize>),
    Column {
        offset: i32,
        column: u32,
        absolute: bool,
        lowercase: bool,
        command: Command,
    },
}

#[derive(Debug)]
enum Command {
    Copy,
    Test(Regex),
    Match(Regex),
}

impl Pattern {
    pub fn compile(source: impl Into<String>) -> Result<Self, CrfError> {
        let mut source = source.into();
        let invalid = |source: &str| CrfError::InvalidPattern(source.to_owned());
        match source.as_bytes().first() {
            Some(b'u' | b'b' | b'*') => {}
            Some(b'U' | b'B') => source[..1].make_ascii_lowercase(),
            _ => return Err(invalid(&source)),
        }
        if source.contains('\0') {
            return Err(invalid(&source));
        }
        let mut items = Vec::new();
        let mut rest = source.as_bytes();
        while !rest.is_empty() {
            if rest[0] != b'%' {
                let start = source.len() - rest.len();
                let len = rest.iter().position(|&b| b == b'%').unwrap_or(rest.len());
                items.push(Item::Literal(start..start + len));
                rest = &rest[len..];
            } else {
                let (item, tail) = compile_command(rest).ok_or_else(|| invalid(&source))?;
                items.push(item);
                rest = tail;
            }
        }
        Ok(Self { source, items })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Replace the contents of the scratch buffer with the observation bytes
    /// for the given row. The output may contain a partial UTF-8 code point
    /// because Wapiti's regex engine matches bytes.
    pub fn execute(
        &self,
        rows: &[&[&str]],
        at: usize,
        output: &mut Vec<u8>,
    ) -> Result<(), CrfError> {
        self.format(rows.len(), at, 0, output, |row, col| {
            rows.get(row)?.get(col).copied()
        })
    }

    pub(crate) fn format<'a>(
        &self,
        len: usize,
        at: usize,
        prefix_len: usize,
        output: &mut Vec<u8>,
        get: impl Fn(usize, usize) -> Option<&'a str>,
    ) -> Result<(), CrfError> {
        const BEFORE: [&[u8]; 5] = [b"_x-1", b"_x-2", b"_x-3", b"_x-4", b"_x-#"];
        const AFTER: [&[u8]; 5] = [b"_x+1", b"_x+2", b"_x+3", b"_x+4", b"_x+#"];
        output.truncate(prefix_len);
        let mut terminator = None;
        for item in &self.items {
            match item {
                Item::Literal(range) => {
                    output.extend_from_slice(&self.source.as_bytes()[range.clone()])
                }
                Item::Column {
                    offset,
                    column,
                    absolute,
                    lowercase,
                    command,
                } => {
                    let offset = i64::from(*offset);
                    let column = *column as usize;
                    let len = len as i64;
                    let row = if !absolute {
                        at as i64 + offset
                    } else if offset < 0 {
                        len + offset
                    } else {
                        offset - 1
                    };
                    let token = if row < 0 {
                        BEFORE[(-row - 1).min(4) as usize]
                    } else if row >= len {
                        AFTER[(row - len).min(4) as usize]
                    } else {
                        let row = row as usize;
                        let value =
                            get(row, column).ok_or(CrfError::MissingColumn { row, column })?;
                        c_bytes(value)
                    };
                    let value: &[u8] = match command {
                        Command::Copy => token,
                        Command::Test(regex) => {
                            if regex.find(token).is_some() {
                                b"true"
                            } else {
                                b"false"
                            }
                        }
                        Command::Match(regex) => match regex.find(token) {
                            Some(range) if range.end > token.len() => {
                                // Optional backtracking can leave extra
                                // length in the match. If it reaches the
                                // token's NUL, Wapiti ends the observation there.
                                let tail = &token[range.start..];
                                terminator.get_or_insert(output.len() + tail.len());
                                tail
                            }
                            Some(range) => &token[range],
                            None => b"",
                        },
                    };
                    let start = output.len();
                    output.extend_from_slice(value);
                    if *lowercase {
                        output[start..].make_ascii_lowercase();
                    }
                }
            }
        }
        if let Some(end) = terminator {
            output.truncate(end);
        }
        Ok(())
    }
}

pub(crate) fn c_bytes(value: &str) -> &[u8] {
    value
        .split_once('\0')
        .map_or(value, |(before, _)| before)
        .as_bytes()
}

fn compile_command(bytes: &[u8]) -> Option<(Item, &[u8])> {
    let (&command, rest) = bytes.get(1..)?.split_first()?;
    let lower = command.to_ascii_lowercase();
    if !matches!(lower, b'x' | b't' | b'm') {
        return None;
    }
    let mut rest = rest.strip_prefix(b"[")?;
    let absolute = rest.starts_with(b"@");
    if absolute {
        rest = &rest[1..];
    }
    let (offset, rest) = scan_integer(rest, true)?;
    let offset = i32::try_from(offset).ok()?;
    let (column, rest) = scan_integer(rest.strip_prefix(b",")?, false)?;
    let column = if column < 0 {
        u32::try_from(-column).ok()?.wrapping_neg()
    } else {
        u32::try_from(column).ok()?
    };
    let (command_type, rest) = if lower == b'x' {
        (Command::Copy, rest)
    } else {
        // Wapiti accepts the separator if either byte is the expected one.
        if *rest.first()? != b',' && *rest.get(1)? != b'"' {
            return None;
        }
        let rest = rest.get(2..)?;
        let mut end = 0;
        while *rest.get(end)? != b'"' {
            end += if rest[end] == b'\\' { 2 } else { 1 };
        }
        let regex = Regex::compile(&rest[..end])?;
        let command = if lower == b't' {
            Command::Test(regex)
        } else {
            Command::Match(regex)
        };
        (command, &rest[end + 1..])
    };
    Some((
        Item::Column {
            offset,
            column,
            absolute,
            lowercase: lower != command,
            command: command_type,
        },
        rest.strip_prefix(b"]")?,
    ))
}

// Wapiti uses scanf %i for offsets and %u for columns.
fn scan_integer(mut bytes: &[u8], auto_radix: bool) -> Option<(i64, &[u8])> {
    while bytes.first().is_some_and(|&b| is_c_space(b)) {
        bytes = &bytes[1..];
    }
    let sign = match bytes.first()? {
        b'-' => {
            bytes = &bytes[1..];
            -1
        }
        b'+' => {
            bytes = &bytes[1..];
            1
        }
        _ => 1,
    };
    let radix = if auto_radix && (bytes.starts_with(b"0x") || bytes.starts_with(b"0X")) {
        bytes = &bytes[2..];
        16
    } else if auto_radix && bytes.starts_with(b"0") {
        8
    } else {
        10
    };
    let end = bytes
        .iter()
        .position(|&b| !(b as char).is_digit(radix))
        .unwrap_or(bytes.len());
    let digits = std::str::from_utf8(&bytes[..end]).ok()?;
    let value = i64::from_str_radix(digits, radix).ok()?;
    Some((sign * value, &bytes[end..]))
}

#[derive(Debug)]
struct Regex {
    anchored: bool,
    end: bool,
    atoms: Vec<Atom>,
}

#[derive(Debug)]
struct Atom {
    matcher: Matcher,
    repeat: Repeat,
}

#[derive(Debug)]
enum Matcher {
    Byte(u8),
    Any,
    Class(u8),
}

impl Matcher {
    fn matches(&self, byte: u8) -> bool {
        match *self {
            Self::Byte(value) => byte == value,
            Self::Any => true,
            Self::Class(class) => {
                let included = match class.to_ascii_lowercase() {
                    b'a' => byte.is_ascii_alphabetic(),
                    b'd' => byte.is_ascii_digit(),
                    b'l' => byte.is_ascii_lowercase(),
                    b'p' => byte.is_ascii_punctuation(),
                    b's' => is_c_space(byte),
                    b'u' => byte.is_ascii_uppercase(),
                    b'w' => byte.is_ascii_alphanumeric(),
                    _ => unreachable!(),
                };
                included ^ class.is_ascii_uppercase()
            }
        }
    }
}

#[derive(Debug)]
enum Repeat {
    One,
    Star,
    Optional,
}

impl Regex {
    fn compile(bytes: &[u8]) -> Option<Self> {
        let anchored = bytes.starts_with(b"^");
        let mut bytes = &bytes[usize::from(anchored)..];
        let mut end = false;
        let mut atoms = Vec::new();
        while let Some((&first, tail)) = bytes.split_first() {
            bytes = tail;
            let matcher = match first {
                b'$' if bytes.is_empty() => {
                    end = true;
                    break;
                }
                b'*' | b'?' => return None,
                b'.' => Matcher::Any,
                b'\\' => {
                    let (&escaped, tail) = bytes.split_first()?;
                    bytes = tail;
                    if matches!(
                        escaped.to_ascii_lowercase(),
                        b'a' | b'd' | b'l' | b'p' | b's' | b'u' | b'w'
                    ) {
                        Matcher::Class(escaped)
                    } else {
                        Matcher::Byte(escaped)
                    }
                }
                byte => Matcher::Byte(byte),
            };
            let repeat = match bytes.first() {
                Some(b'*') => {
                    bytes = &bytes[1..];
                    Repeat::Star
                }
                Some(b'?') => {
                    bytes = &bytes[1..];
                    Repeat::Optional
                }
                _ => Repeat::One,
            };
            atoms.push(Atom { matcher, repeat });
        }
        Some(Self {
            anchored,
            end,
            atoms,
        })
    }

    fn find(&self, token: &[u8]) -> Option<Range<usize>> {
        let last = if self.anchored { 0 } else { token.len() };
        for start in 0..=last {
            let mut length = 0;
            if self.match_here(&self.atoms, token, start, &mut length) {
                return Some(start..start + length);
            }
        }
        None
    }

    fn match_here(
        &self,
        mut atoms: &[Atom],
        token: &[u8],
        mut pos: usize,
        length: &mut usize,
    ) -> bool {
        while let Some((atom, rest)) = atoms.split_first() {
            let matches = |pos| token.get(pos).is_some_and(|&b| atom.matcher.matches(b));
            match atom.repeat {
                Repeat::One => {
                    *length += 1;
                    if !matches(pos) {
                        return false;
                    }
                    pos += 1;
                }
                Repeat::Optional => {
                    if matches(pos) {
                        *length += 1;
                        if self.match_here(rest, token, pos + 1, length) {
                            return true;
                        }
                        // Only the optional atom's own byte is rolled back.
                        *length -= 1;
                    }
                }
                Repeat::Star => loop {
                    // A star tries the shortest match first in Wapiti.
                    let saved = *length;
                    if self.match_here(rest, token, pos, length) {
                        return true;
                    }
                    *length = saved + 1;
                    if !matches(pos) {
                        return false;
                    }
                    pos += 1;
                },
            }
            atoms = rest;
        }
        !self.end || pos == token.len()
    }
}
