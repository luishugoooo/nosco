use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek},
    path::Path,
};

use flate2::bufread::MultiGzDecoder;

use crate::{BigramWeight, CrfError, CrfModel, ModelType, Pattern, UnigramWeight, parse_hex_float};

impl CrfModel {
    /// Load plain or gzip-compressed Wapiti text. The loader scans the
    /// observation section twice and retains only the active observations.
    pub fn load_wapiti(path: impl AsRef<Path>) -> Result<Self, CrfError> {
        load_path(path.as_ref(), false)
    }

    /// Load Wapiti text or gzip from an arbitrary reader. A forward-only
    /// stream needs a temporary byte arena for replaying the observations.
    pub fn from_wapiti_reader(reader: impl Read) -> Result<Self, CrfError> {
        let mut reader = BufReader::new(reader);
        // A Read implementation is allowed to deliver the magic one byte at a time.
        let mut prefix = [0; 2];
        reader.read_exact(&mut prefix)?;
        let replay = prefix.as_slice().chain(reader);
        if prefix == GZIP_MAGIC {
            load_stream(BufReader::new(MultiGzDecoder::new(BufReader::new(replay))))
        } else {
            load_stream(BufReader::new(replay))
        }
    }
}

const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const INVALID_TEXT: CrfError = CrfError::InvalidModel("malformed Wapiti model");

pub(crate) fn load_path(path: &Path, auto: bool) -> Result<CrfModel, CrfError> {
    let mut reader = BufReader::new(File::open(path)?);
    let head = reader.fill_buf()?;
    if auto && head.starts_with(crate::binary::MAGIC) {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        CrfModel::from_gcrf_bytes(&bytes)
    } else if head.starts_with(&GZIP_MAGIC) {
        let reader = BufReader::new(MultiGzDecoder::new(reader));
        load_two_pass(reader, |reader| {
            let mut raw = reader.into_inner().into_inner();
            raw.rewind()?;
            Ok(BufReader::new(MultiGzDecoder::new(raw)))
        })
    } else {
        load_two_pass(reader, |mut reader| {
            reader.rewind()?;
            Ok(reader)
        })
    }
}

fn load_two_pass<R: BufRead>(
    mut reader: R,
    rewind: impl FnOnce(R) -> Result<R, CrfError>,
) -> Result<CrfModel, CrfError> {
    let (model, active, count) = read_header(&mut reader)?;
    let features = scan_observations(&mut reader, count, model.num_labels(), |_| {})?;
    let weights = read_weights(&mut reader, active, features)?;
    if weights.is_empty() {
        return compact(model, &weights, count, |_| unreachable!());
    }
    let mut reader = rewind(reader)?;
    read_header(&mut reader)?;
    compact(model, &weights, count, |buffer| {
        read_netstring(&mut reader, buffer)
    })
}

fn load_stream(mut reader: impl BufRead) -> Result<CrfModel, CrfError> {
    let (model, active, count) = read_header(&mut reader)?;
    let mut arena = Vec::new();
    let features = scan_observations(&mut reader, count, model.num_labels(), |observation| {
        if active != 0 {
            arena.extend_from_slice(&(observation.len() as u32).to_le_bytes());
            arena.extend_from_slice(observation);
        }
    })?;
    let weights = read_weights(&mut reader, active, features)?;
    let mut remaining = arena.as_slice();
    compact(model, &weights, count, |buffer| {
        let (length, tail) = remaining.split_at(4);
        let length = u32::from_le_bytes(length.try_into().unwrap()) as usize;
        let (obs, tail) = tail.split_at(length);
        remaining = tail;
        buffer.clear();
        buffer.extend_from_slice(obs);
        Ok(())
    })
}

fn scan_observations(
    reader: &mut impl BufRead,
    count: u64,
    labels: usize,
    mut visit: impl FnMut(&[u8]),
) -> Result<u64, CrfError> {
    let mut buffer = Vec::new();
    let mut features = 0;
    for _ in 0..count {
        read_netstring(reader, &mut buffer)?;
        features = Layout::new(features, labels, &buffer)?.end;
        visit(&buffer);
    }
    Ok(features)
}

fn read_header(reader: &mut impl BufRead) -> Result<(CrfModel, usize, u64), CrfError> {
    let mut line = String::new();
    let mut parts = read_line(reader, &mut line)?
        .strip_prefix("#mdl#")
        .ok_or(INVALID_TEXT)?
        .split('#');
    let first = parts.next().ok_or(INVALID_TEXT)?;
    let (model_type, active) = match parts.next() {
        Some(active) => (ModelType::try_from(parse::<u8>(first)?)?, parse(active)?),
        None => (ModelType::MaxEnt, parse(first)?),
    };
    if parts.next().is_some() {
        return Err(INVALID_TEXT);
    }
    let mut parts = read_line(reader, &mut line)?
        .strip_prefix("#rdr#")
        .ok_or(INVALID_TEXT)?
        .split('/');
    let count: u32 = parse(parts.next().ok_or(INVALID_TEXT)?)?;
    let _: u32 = parse(parts.next().ok_or(INVALID_TEXT)?)?;
    // Older readers default to non-autouni when no flag was stored.
    let autouni = parts.next().map(parse::<i32>).transpose()?.unwrap_or(0) != 0;
    if parts.next().is_some() {
        return Err(INVALID_TEXT);
    }
    let mut buffer = Vec::new();
    let mut patterns = Vec::new();
    for _ in 0..count {
        read_netstring(reader, &mut buffer)?;
        patterns.push(Pattern::compile(as_str(&buffer)?)?);
    }
    let count = parse_count(read_line(reader, &mut line)?)?;
    if !(1..=u64::from(u16::MAX) + 1).contains(&count) {
        return Err(CrfError::InvalidModel("invalid label count"));
    }
    let mut labels = Vec::new();
    for _ in 0..count {
        read_netstring(reader, &mut buffer)?;
        labels.push(as_str(&buffer)?.to_owned());
    }
    let observations = parse_count(read_line(reader, &mut line)?)?;
    Ok((
        CrfModel {
            model_type,
            autouni,
            labels,
            patterns,
            observations: Default::default(),
            unigram: Default::default(),
            bigram: Default::default(),
        },
        active,
        observations,
    ))
}

fn read_line<'a>(reader: &mut impl BufRead, line: &'a mut String) -> Result<&'a str, CrfError> {
    line.clear();
    if reader.read_line(line)? == 0 {
        return Err(INVALID_TEXT);
    }
    Ok(line.trim_matches(|c: char| c.is_ascii() && crate::is_c_space(c as u8)))
}

fn parse<T: std::str::FromStr>(value: &str) -> Result<T, CrfError> {
    value.parse().map_err(|_| INVALID_TEXT)
}

fn parse_count(line: &str) -> Result<u64, CrfError> {
    parse(line.strip_prefix("#qrk#").ok_or(INVALID_TEXT)?)
}

pub(crate) fn as_str(bytes: &[u8]) -> Result<&str, CrfError> {
    if bytes.contains(&0) {
        return Err(CrfError::InvalidModel("unexpected NUL byte"));
    }
    std::str::from_utf8(bytes).map_err(|_| CrfError::InvalidModel("expected UTF-8 text"))
}

fn read_netstring(reader: &mut impl BufRead, output: &mut Vec<u8>) -> Result<(), CrfError> {
    let mut len = 0u32;
    let mut digit_seen = false;
    'prefix: loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Err(INVALID_TEXT);
        }
        for (i, &byte) in bytes.iter().enumerate() {
            match byte {
                b'0'..=b'9' => {
                    digit_seen = true;
                    len = len
                        .checked_mul(10)
                        .and_then(|n| n.checked_add(u32::from(byte - b'0')))
                        .ok_or(CrfError::TooLarge)?;
                }
                b':' if digit_seen => {
                    reader.consume(i + 1);
                    break 'prefix;
                }
                _ => return Err(INVALID_TEXT),
            }
        }
        let consumed = bytes.len();
        reader.consume(consumed);
    }
    output.clear();
    let mut remaining = len as usize;
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Err(INVALID_TEXT);
        }
        let n = remaining.min(bytes.len());
        output.extend_from_slice(&bytes[..n]);
        remaining -= n;
        if bytes[n..].starts_with(b",\n") {
            reader.consume(n + 2);
            return Ok(());
        }
        reader.consume(n);
        if remaining == 0 {
            break;
        }
    }
    let mut delimiter = [0; 2];
    reader.read_exact(&mut delimiter)?;
    if delimiter == *b",\r" {
        reader.read_exact(&mut delimiter[..1])?;
        delimiter = [b',', delimiter[0]];
    }
    if delimiter != *b",\n" {
        return Err(INVALID_TEXT);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Layout {
    bigram_start: u64,
    end: u64,
}

impl Layout {
    fn new(start: u64, labels: usize, observation: &[u8]) -> Result<Self, CrfError> {
        let labels = labels as u64;
        let (uni, bi) = match observation.first() {
            Some(b'u') => (labels, 0),
            Some(b'b') => (0, labels * labels),
            Some(b'*') => (labels, labels * labels),
            _ => return Err(CrfError::InvalidModel("invalid observation prefix")),
        };
        let end = start.checked_add(uni + bi).ok_or(CrfError::TooLarge)?;
        Ok(Self {
            bigram_start: start + uni,
            end,
        })
    }
}

#[derive(Clone, Copy)]
struct Feature {
    id: u64,
    weight: f64,
}

fn read_weights(
    reader: &mut impl BufRead,
    active: usize,
    features: u64,
) -> Result<Vec<Feature>, CrfError> {
    let mut line = String::new();
    let mut weights = Vec::new();
    for _ in 0..active {
        let (id, weight) = read_line(reader, &mut line)?
            .split_once('=')
            .ok_or(INVALID_TEXT)?;
        let id = parse(id)?;
        let weight = parse_hex_float(weight)?;
        if id >= features || !weight.is_finite() {
            return Err(CrfError::InvalidModel("invalid feature index or weight"));
        }
        weights.push(Feature { id, weight });
    }
    if !weights.windows(2).all(|pair| pair[0].id < pair[1].id) {
        // The text loader in C applies assignments in file order.
        weights.sort_by_key(|f| f.id);
        weights.dedup_by(|later, earlier| {
            if later.id == earlier.id {
                earlier.weight = later.weight;
                true
            } else {
                false
            }
        });
    }
    weights.retain(|f| f.weight != 0.0);

    // Reach EOF to validate the gzip trailer and reject unexpected extra data.
    loop {
        let tail = reader.fill_buf()?;
        if tail.is_empty() {
            break;
        }
        if !tail.iter().copied().all(crate::is_c_space) {
            return Err(INVALID_TEXT);
        }
        let len = tail.len();
        reader.consume(len);
    }
    Ok(weights)
}

fn compact(
    mut model: CrfModel,
    mut pending: &[Feature],
    count: u64,
    mut read_observation: impl FnMut(&mut Vec<u8>) -> Result<(), CrfError>,
) -> Result<CrfModel, CrfError> {
    let mut buffer = Vec::new();
    let mut start = 0;
    for _ in 0..count {
        if pending.is_empty() {
            break;
        }
        read_observation(&mut buffer)?;
        model.compact_row(&buffer, &mut start, &mut pending)?;
    }
    if !pending.is_empty() {
        return Err(INVALID_TEXT);
    }
    model.observations.build_table()?;
    Ok(model)
}

impl CrfModel {
    fn compact_row(
        &mut self,
        observation: &[u8],
        start: &mut u64,
        pending: &mut &[Feature],
    ) -> Result<(), CrfError> {
        let labels = self.num_labels() as u64;
        let layout = Layout::new(*start, labels as usize, observation)?;
        if pending[0].id < layout.end {
            self.observations.push(observation)?;
            while let Some((feature, rest)) = pending.split_first() {
                if feature.id >= layout.end {
                    break;
                }
                let weight = feature.weight;
                if feature.id < layout.bigram_start {
                    self.unigram.weights.push(UnigramWeight {
                        label: (feature.id - *start) as u16,
                        weight,
                    });
                } else {
                    let index = feature.id - layout.bigram_start;
                    self.bigram.weights.push(BigramWeight {
                        prev: (index / labels) as u16,
                        cur: (index % labels) as u16,
                        weight,
                    });
                }
                *pending = rest;
            }
            self.unigram.end_row()?;
            self.bigram.end_row()?;
        }
        *start = layout.end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netstrings_can_cross_every_buffer_boundary() {
        let data = b"12:u:one,\n\ntwo!,\n4:b:xx,\r\n0:,\n";
        for capacity in 1..=data.len() {
            let mut reader = BufReader::with_capacity(capacity, &data[..]);
            let mut buffer = Vec::new();
            read_netstring(&mut reader, &mut buffer).unwrap();
            assert_eq!(buffer, b"u:one,\n\ntwo!");
            read_netstring(&mut reader, &mut buffer).unwrap();
            assert_eq!(buffer, b"b:xx");
            read_netstring(&mut reader, &mut buffer).unwrap();
            assert!(buffer.is_empty());
            assert!(reader.fill_buf().unwrap().is_empty());
        }
    }
}
