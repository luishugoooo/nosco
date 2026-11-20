//! Read the encoding and glyph names of a name-keyed Compact Font Format font.

use std::sync::OnceLock;

pub(crate) enum Encoding<'a> {
    Standard,
    Expert,
    Custom(Box<[Option<&'a [u8]>; 256]>),
}

pub(crate) fn encoding(font: &[u8]) -> Option<Encoding<'_>> {
    let font = if font.starts_with(b"OTTO") {
        crate::truetype::table(font, b"CFF ")?
    } else {
        font
    };
    let [1, _, header_len, _, ..] = *font else {
        return None;
    };
    let mut data = font.get(usize::from(header_len)..)?;
    Index::read(&mut data)?; // Name INDEX
    let dict = Index::read(&mut data)?;
    let strings = Index::read(&mut data)?;
    let (charset_offset, encoding_offset, charstrings_offset) = offsets(dict.get(0)?)?;
    match encoding_offset {
        0 => return Some(Encoding::Standard),
        1 => return Some(Encoding::Expert),
        _ => {}
    }
    let glyph_count = Index::read(&mut font.get(charstrings_offset..)?)?.count();
    let charset = charset(font, charset_offset, glyph_count)?;
    let name = |sid: u16| {
        let sid = usize::from(sid);
        let standard = &standard_data().strings;
        if sid < standard.len() {
            Some(standard[sid])
        } else {
            strings.get(sid - standard.len())
        }
    };

    let mut codes = [None; 256];
    let mut data = font.get(encoding_offset..)?;
    let [format, count] = take(&mut data)?;
    let mut glyphs = charset.into_iter().skip(1);
    for _ in 0..count {
        let [code] = take(&mut data)?;
        let additional = match format & 0x7f {
            0 => 0,
            1 => take::<1>(&mut data)?[0],
            _ => return None,
        };
        let end = code.checked_add(additional)?;
        for code in code..=end {
            codes[usize::from(code)] = name(glyphs.next()?);
        }
    }
    if format & 0x80 != 0 {
        let [count] = take(&mut data)?;
        for _ in 0..count {
            let [code, hi, lo] = take(&mut data)?;
            codes[usize::from(code)] = name(u16::from_be_bytes([hi, lo]));
        }
    }
    Some(Encoding::Custom(Box::new(codes)))
}

fn take<const N: usize>(data: &mut &[u8]) -> Option<[u8; N]> {
    let (head, tail) = data.split_at_checked(N)?;
    *data = tail;
    head.try_into().ok()
}

struct Index<'a> {
    offsets: &'a [u8],
    offset_size: usize,
    data: &'a [u8],
}

impl<'a> Index<'a> {
    fn read(input: &mut &'a [u8]) -> Option<Self> {
        let count = usize::from(u16::from_be_bytes(take(input)?));
        if count == 0 {
            return Some(Self {
                offsets: &[],
                offset_size: 1,
                data: &[],
            });
        }
        let size = usize::from(take::<1>(input)?[0]);
        if !(1..=4).contains(&size) {
            return None;
        }
        let (offsets, remaining) = input.split_at_checked((count + 1) * size)?;
        let offset = read_offset(offsets.get(count * size..)?)?;
        let (data, rest) = remaining.split_at_checked(offset)?;
        *input = rest;
        Some(Self {
            offsets,
            offset_size: size,
            data,
        })
    }

    fn count(&self) -> usize {
        (self.offsets.len() / self.offset_size).saturating_sub(1)
    }

    fn get(&self, index: usize) -> Option<&'a [u8]> {
        if index >= self.count() {
            return None;
        }
        let start = index * self.offset_size;
        let middle = start + self.offset_size;
        let last = middle + self.offset_size;
        self.data.get(
            read_offset(&self.offsets[start..middle])?..read_offset(&self.offsets[middle..last])?,
        )
    }
}

fn read_offset(bytes: &[u8]) -> Option<usize> {
    bytes
        .iter()
        .fold(0usize, |value, &byte| (value << 8) | usize::from(byte))
        .checked_sub(1)
}

fn offsets(mut dict: &[u8]) -> Option<(usize, usize, usize)> {
    let (mut charset, mut encoding, mut charstrings) = (0, 0, None);
    let mut last = None;
    while let Some((&byte, rest)) = dict.split_first() {
        dict = rest;
        match byte {
            0..=27 => {
                let offset = || last.and_then(|value| usize::try_from(value).ok());
                match byte {
                    12 => {
                        let [escaped] = take(&mut dict)?;
                        if escaped == 30 {
                            return None; // A CID-keyed charset contains CIDs rather than names.
                        }
                    }
                    15 => charset = offset()?,
                    16 => encoding = offset()?,
                    17 => charstrings = offset(),
                    _ => {}
                }
                last = None;
            }
            28 => last = Some(i32::from(i16::from_be_bytes(take(&mut dict)?))),
            29 => last = Some(i32::from_be_bytes(take(&mut dict)?)),
            30 => {
                let end = dict
                    .iter()
                    .position(|&byte| byte & 0x0f == 0x0f || byte >> 4 == 0x0f)?;
                dict = &dict[end + 1..];
                last = None;
            }
            32..=246 => last = Some(i32::from(byte) - 139),
            247..=250 => {
                last =
                    Some((i32::from(byte) - 247) * 256 + i32::from(take::<1>(&mut dict)?[0]) + 108)
            }
            251..=254 => {
                last =
                    Some(-(i32::from(byte) - 251) * 256 - i32::from(take::<1>(&mut dict)?[0]) - 108)
            }
            _ => return None,
        }
    }
    Some((charset, encoding, charstrings?))
}

fn charset(font: &[u8], offset: usize, glyph_count: usize) -> Option<Vec<u16>> {
    match offset {
        0 => return Some((0..(glyph_count.min(229) as u16)).collect()),
        1 | 2 => {
            return Some(
                standard_data()
                    .charsets
                    .get(offset - 1)?
                    .iter()
                    .copied()
                    .take(glyph_count)
                    .collect(),
            );
        }
        _ => {}
    }
    let mut data = font.get(offset..)?;
    let [format] = take(&mut data)?;
    let mut glyphs = Vec::with_capacity(glyph_count);
    glyphs.push(0); // GID 0 is .notdef.
    while glyphs.len() < glyph_count {
        let first = u16::from_be_bytes(take(&mut data)?);
        let count = match format {
            0 => 0,
            1 => u16::from(take::<1>(&mut data)?[0]),
            2 => u16::from_be_bytes(take(&mut data)?),
            _ => return None,
        };
        let last = first.checked_add(count)?;
        glyphs.extend((first..=last).take(glyph_count - glyphs.len()));
    }
    Some(glyphs)
}

struct StandardData {
    strings: Vec<&'static [u8]>,
    charsets: Vec<Vec<u16>>,
}

fn standard_data() -> &'static StandardData {
    static DATA: OnceLock<StandardData> = OnceLock::new();
    DATA.get_or_init(|| {
        let mut strings = Vec::new();
        let mut charsets = Vec::new();
        for line in include_str!("../data/cff-strings.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            if let Some((_, ids)) = line.split_once(' ') {
                charsets.push(
                    ids.split_ascii_whitespace()
                        .map(|id| id.parse().expect("CFF charset SID"))
                        .collect(),
                );
            } else {
                strings.push(line.as_bytes());
            }
        }
        StandardData { strings, charsets }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_encoding_resolves_custom_names_and_supplements() {
        let data = [
            1, 0, 4, 1, // header
            0, 1, 1, 1, 2, b'F', // name index
            0, 1, 1, 1, 7, 172, 15, 177, 16, 185,
            17, // dict: charset 33, encoding 38, charstrings 46
            0, 1, 1, 1, 6, b'u', b'2', b'6', b'0', b'3', // string index, SID 391
            0, 0, // global subrs
            0, 1, 135, 0, 34, // charset: .notdef, u2603, A
            0x81, 1, 65, 1, 1, 90, 0,  // encoding ranges and one supplement
            34, // supplement SID 34 (A)
            0, 3, 1, 1, 2, 3, 4, 14, 14, 14, // glyphs
        ];
        let Some(Encoding::Custom(codes)) = encoding(&data) else {
            panic!("no custom encoding")
        };
        assert_eq!(codes[65], Some(b"u2603".as_slice()));
        assert_eq!(codes[66], Some(b"A".as_slice()));
        assert_eq!(codes[90], Some(b"A".as_slice()));
    }
}
