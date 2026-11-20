//! Unicode lookup for CIDFontType2 fonts that omit a ToUnicode map.

fn word(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn long(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

pub(crate) fn table<'a>(font: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let count = usize::from(word(font, 4)?);
    for record in font.get(12..)?.as_chunks::<16>().0.iter().take(count) {
        if record.starts_with(tag) {
            let offset = long(record, 8)? as usize;
            let length = long(record, 12)? as usize;
            return font.get(offset..offset.checked_add(length)?);
        }
    }
    None
}

pub(crate) fn unicode_by_glyph(font: &[u8]) -> Option<Box<[Option<char>]>> {
    let count = usize::from(word(table(font, b"maxp")?, 4)?);
    let cmap = table(font, b"cmap")?;
    let records = usize::from(word(cmap, 2)?);
    let mut maps: Vec<_> = cmap
        .get(4..)?
        .as_chunks::<8>()
        .0
        .iter()
        .take(records)
        .filter_map(|record| {
            let platform = word(record, 0)?;
            let encoding = word(record, 2)?;
            if platform != 0 && !(platform == 3 && matches!(encoding, 1 | 10)) {
                return None;
            }
            let map = cmap.get((long(record, 4)? as usize)..)?;
            let format = word(map, 0)?;
            let priority = match format {
                12 => 0,
                4 => 1,
                6 => 2,
                0 => 3,
                _ => return None,
            };
            Some((priority, format, map))
        })
        .collect();
    if maps.is_empty() {
        return None;
    }
    maps.sort_by_key(|entry| entry.0);
    maps.dedup_by(|a, b| std::ptr::eq(a.2, b.2));
    let mut result = vec![None; count].into_boxed_slice();
    let mut add = |gid: u32, value: u32| {
        if gid != 0
            && let Some(slot @ None) = result.get_mut(gid as usize)
            && let Some(ch) = char::from_u32(value)
            && !ch.is_control()
            && !matches!(value, 0xe000..=0xf8ff | 0xf0000..=0x10ffff | 0xffff)
        {
            *slot = Some(ch);
        }
    };
    for (_, format, map) in maps {
        match format {
            12 => {
                let length = long(map, 4)? as usize;
                let groups = long(map, 12)? as usize;
                for group in map.get(16..length)?.as_chunks::<12>().0.iter().take(groups) {
                    let first = long(group, 0)?;
                    let last = long(group, 4)?.min(0x10ffff);
                    let gid = long(group, 8)?;
                    if first <= last && gid < count as u32 {
                        let len = (last - first).min(count as u32 - gid - 1);
                        for offset in 0..=len {
                            add(gid + offset, first + offset);
                        }
                    }
                }
            }
            4 => {
                let length = usize::from(word(map, 2)?);
                let map = map.get(..length)?;
                let count = usize::from(word(map, 6)?) / 2;
                let starts = 16 + 2 * count;
                let deltas = starts + 2 * count;
                let offsets = deltas + 2 * count;
                let mut next = 0u32;
                for index in (0..2 * count).step_by(2) {
                    let first = u32::from(word(map, starts + index)?).max(next);
                    let last = u32::from(word(map, 14 + index)?);
                    let delta = word(map, deltas + index)?;
                    let offset = usize::from(word(map, offsets + index)?);
                    let original_start = u32::from(word(map, starts + index)?);
                    for value in first..=last {
                        let gid = if offset == 0 {
                            (value as u16).wrapping_add(delta)
                        } else {
                            let index =
                                offsets + index + offset + 2 * (value - original_start) as usize;
                            let Some(glyph) = word(map, index) else { break };
                            if glyph == 0 {
                                0
                            } else {
                                glyph.wrapping_add(delta)
                            }
                        };
                        add(u32::from(gid), value);
                    }
                    next = next.max(last + 1);
                }
            }
            6 => {
                let start = u32::from(word(map, 6)?);
                let count = usize::from(word(map, 8)?);
                for (offset, glyph) in map
                    .get(10..)?
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .take(count)
                    .enumerate()
                {
                    add(
                        u32::from(u16::from_be_bytes([glyph[0], glyph[1]])),
                        start + offset as u32,
                    );
                }
            }
            0 => {
                for (value, &glyph) in map.get(6..)?.iter().take(256).enumerate() {
                    add(u32::from(glyph), value as u32);
                }
            }
            _ => {}
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_cmap_maps_cid_glyphs_to_astral_unicode() {
        let mut font = vec![0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0];
        for (tag, offset, length) in [(b"maxp", 44u32, 6u32), (b"cmap", 50, 52)] {
            font.extend_from_slice(tag);
            font.extend_from_slice(&[0; 4]);
            font.extend_from_slice(&offset.to_be_bytes());
            font.extend_from_slice(&length.to_be_bytes());
        }
        font.extend_from_slice(&[0, 1, 0, 0, 0, 4]); // maxp: four glyphs
        font.extend_from_slice(&[0, 0, 0, 1, 0, 3, 0, 10, 0, 0, 0, 12]); // Unicode record
        font.extend_from_slice(&[0, 12, 0, 0]); // format 12
        for value in [40u32, 0, 2, 0x41, 0x42, 1, 0x1f642, 0x1f642, 3] {
            font.extend_from_slice(&value.to_be_bytes());
        }
        let glyphs = unicode_by_glyph(&font).unwrap();
        assert_eq!(&*glyphs, &[None, Some('A'), Some('B'), Some('🙂')]);
    }
}
