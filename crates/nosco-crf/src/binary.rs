use std::{
    fs::{self, OpenOptions},
    io::{self, BufWriter, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::wapiti::as_str;
use crate::{
    BigramWeight, CrfError, CrfModel, ModelType, Observations, Pattern, Sparse, UnigramWeight,
};

pub(crate) const MAGIC: &[u8; 4] = b"GCRF";
const VERSION: u32 = 1;
const INVALID: CrfError = CrfError::InvalidModel("malformed GCRF data");

// Version 1 uses little-endian integers and IEEE binary64 throughout:
// magic, version, type and autouni (one byte each), six u32 counts (labels,
// patterns, observations, pool bytes, unigram weights, bigram weights),
// length-prefixed labels and patterns, pool, observation offsets, hash
// buckets, unigram offsets, bigram offsets, unigram records (u16 + f64),
// bigram records (u16 + u16 + f64), and an XXH3-64 checksum of everything
// preceding it. Each offset array includes its terminal offset.
impl CrfModel {
    pub fn save_gcrf(&self, path: impl AsRef<Path>) -> Result<(), CrfError> {
        static SAVE_ID: AtomicU64 = AtomicU64::new(0);
        let path = path.as_ref();
        loop {
            // Write beside the destination so a successful rename publishes
            // the complete file in one operation.
            let id = SAVE_ID.fetch_add(1, Ordering::Relaxed);
            let temporary = path.with_file_name(format!(".gcrf-{}-{id}.tmp", std::process::id()));
            let file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                result => result?,
            };
            let result = self
                .write_gcrf(file)
                .and_then(|()| Ok(fs::rename(&temporary, path)?));
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            return result;
        }
    }

    pub fn write_gcrf(&self, mut writer: impl Write) -> Result<(), CrfError> {
        let mut checksum = Xxh3::new();
        {
            let mut out = BufWriter::new(ChecksumWriter {
                writer: &mut writer,
                checksum: &mut checksum,
            });
            out.write_all(MAGIC)?;
            put_u32(&mut out, VERSION as usize)?;
            out.write_all(&[self.model_type as u8, u8::from(self.autouni)])?;
            for count in [
                self.num_labels(),
                self.patterns.len(),
                self.observations.len(),
                self.observations.bytes.len(),
                self.unigram.weights.len(),
                self.bigram.weights.len(),
            ] {
                put_u32(&mut out, count)?;
            }
            for item in self
                .labels
                .iter()
                .map(String::as_str)
                .chain(self.patterns.iter().map(Pattern::as_str))
            {
                put_u32(&mut out, item.len())?;
                out.write_all(item.as_bytes())?;
            }
            out.write_all(&self.observations.bytes)?;
            for section in [
                &self.observations.offsets,
                &self.observations.table,
                &self.unigram.offsets,
                &self.bigram.offsets,
            ] {
                for value in section {
                    out.write_all(&value.to_le_bytes())?;
                }
            }
            for weight in &self.unigram.weights {
                out.write_all(&weight.label.to_le_bytes())?;
                out.write_all(&weight.weight.to_le_bytes())?;
            }
            for weight in &self.bigram.weights {
                out.write_all(&weight.prev.to_le_bytes())?;
                out.write_all(&weight.cur.to_le_bytes())?;
                out.write_all(&weight.weight.to_le_bytes())?;
            }
            out.flush()?;
        }
        writer.write_all(&checksum.digest().to_le_bytes())?;
        writer.flush()?;
        Ok(())
    }

    pub fn load_gcrf(path: impl AsRef<Path>) -> Result<Self, CrfError> {
        Self::from_gcrf_bytes(&fs::read(path)?)
    }

    /// Load a GCRF file using an owned input buffer. This is the portable
    /// fallback for callers that use the mmap loading entry point.
    pub fn load_gcrf_mmap(path: impl AsRef<Path>) -> Result<Self, CrfError> {
        Self::load_gcrf(path)
    }

    pub fn from_gcrf_bytes(bytes: &[u8]) -> Result<Self, CrfError> {
        let (payload, checksum) = bytes.split_last_chunk::<8>().ok_or(INVALID)?;
        let mut input = Input(payload);
        if input.take(4)? != MAGIC {
            return Err(INVALID);
        }
        let version = input.u32()?;
        if version != VERSION {
            return Err(CrfError::UnsupportedVersion(version));
        }
        if &xxh3_64(payload).to_le_bytes() != checksum {
            return Err(CrfError::ChecksumMismatch);
        }

        let model_type = ModelType::try_from(input.take(1)?[0])?;
        let autouni = match input.take(1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(INVALID),
        };
        let nlabels = input.count()?;
        let npatterns = input.count()?;
        let nobs = input.count()?;
        let pool_bytes = input.count()?;
        let nunigrams = input.count()?;
        let nbigrams = input.count()?;
        if !(1..=usize::from(u16::MAX) + 1).contains(&nlabels) {
            return Err(INVALID);
        }

        let labels = (0..nlabels)
            .map(|_| Ok(input.string()?.to_owned()))
            .collect::<Result<Vec<_>, CrfError>>()?;
        let patterns = (0..npatterns)
            .map(|_| Pattern::compile(input.string()?))
            .collect::<Result<Vec<_>, _>>()?;
        let pool = input.take(pool_bytes)?;
        let offset_count = nobs.checked_add(1).ok_or(CrfError::TooLarge)?;
        let offsets = input.offsets(offset_count, pool_bytes)?;
        let table_size = Observations::capacity_for(nobs)?;
        let hash_table = input.array(table_size, 4)?;
        let uni_offsets = input.offsets(offset_count, nunigrams)?;
        let bi_offsets = input.offsets(offset_count, nbigrams)?;
        let uni_weights = input.array(nunigrams, 10)?;
        let bi_weights = input.array(nbigrams, 12)?;
        if !input.0.is_empty() {
            return Err(INVALID);
        }

        let mut observations = Observations {
            bytes: pool.to_owned(),
            offsets,
            table: Vec::new(),
        };
        let unigram = Sparse {
            offsets: uni_offsets,
            weights: uni_weights
                .as_chunks::<10>()
                .0
                .iter()
                .map(|entry| UnigramWeight {
                    label: u16::from_le_bytes(entry[..2].try_into().unwrap()),
                    weight: f64::from_le_bytes(entry[2..].try_into().unwrap()),
                })
                .collect(),
        };
        let bigram = Sparse {
            offsets: bi_offsets,
            weights: bi_weights
                .as_chunks::<12>()
                .0
                .iter()
                .map(|entry| BigramWeight {
                    prev: u16::from_le_bytes(entry[..2].try_into().unwrap()),
                    cur: u16::from_le_bytes(entry[2..4].try_into().unwrap()),
                    weight: f64::from_le_bytes(entry[4..].try_into().unwrap()),
                })
                .collect(),
        };
        for id in 0..nobs {
            let uni = unigram.get(id);
            let bi = bigram.get(id);
            let valid_kind = match observations.get(id).first() {
                Some(b'u') => bi.is_empty(),
                Some(b'b') => uni.is_empty(),
                Some(b'*') => true,
                _ => false,
            };
            if !valid_kind
                || (uni.is_empty() && bi.is_empty())
                || uni
                    .iter()
                    .any(|w| usize::from(w.label) >= nlabels || !valid_weight(w.weight))
                || bi
                    .iter()
                    .any(|w| usize::from(w.prev.max(w.cur)) >= nlabels || !valid_weight(w.weight))
                || !uni.windows(2).all(|w| w[0].label < w[1].label)
                || !bi
                    .windows(2)
                    .all(|w| (w[0].prev, w[0].cur) < (w[1].prev, w[1].cur))
            {
                return Err(INVALID);
            }
        }
        observations.build_table()?;
        if !observations
            .table
            .iter()
            .zip(hash_table.as_chunks::<4>().0)
            .all(|(entry, serialized)| &entry.to_le_bytes() == serialized)
        {
            return Err(INVALID);
        }
        Ok(Self {
            model_type,
            autouni,
            labels,
            patterns,
            observations,
            unigram,
            bigram,
        })
    }
}

fn valid_weight(weight: f64) -> bool {
    weight != 0.0 && weight.is_finite()
}

fn put_u32(writer: &mut impl Write, value: usize) -> Result<(), CrfError> {
    let value = u32::try_from(value).map_err(|_| CrfError::TooLarge)?;
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

struct ChecksumWriter<'a, W> {
    writer: &'a mut W,
    checksum: &'a mut Xxh3,
}

impl<W: Write> Write for ChecksumWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.writer.write(buffer)?;
        self.checksum.update(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

struct Input<'a>(&'a [u8]);

impl<'a> Input<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], CrfError> {
        let (head, tail) = self.0.split_at_checked(len).ok_or(INVALID)?;
        self.0 = tail;
        Ok(head)
    }

    fn u32(&mut self) -> Result<u32, CrfError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn count(&mut self) -> Result<usize, CrfError> {
        Ok(self.u32()? as usize)
    }

    fn string(&mut self) -> Result<&'a str, CrfError> {
        let len = self.count()?;
        as_str(self.take(len)?)
    }

    fn array(&mut self, count: usize, width: usize) -> Result<&'a [u8], CrfError> {
        self.take(count.checked_mul(width).ok_or(CrfError::TooLarge)?)
    }

    fn offsets(&mut self, count: usize, end: usize) -> Result<Vec<u32>, CrfError> {
        let offsets: Vec<_> = self
            .array(count, 4)?
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(u32::from_le_bytes)
            .collect();
        if offsets.first() != Some(&0)
            || offsets.last().map(|&v| v as usize) != Some(end)
            || !offsets.windows(2).all(|pair| pair[0] <= pair[1])
        {
            return Err(INVALID);
        }
        Ok(offsets)
    }
}
