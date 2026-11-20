//! Wapiti-compatible linear-chain inference and compact model storage.

mod binary;
mod decode;
mod number;
mod pattern;
mod wapiti;

use std::{io, path::Path};

use thiserror::Error;
use xxhash_rust::xxh3::xxh3_64;

pub use decode::{DecodeResult, DecodeScratch};
pub use number::parse_hex_float;
pub use pattern::Pattern;

/// An invalid model, pattern, or input sequence, or an I/O failure.
#[derive(Debug, Error)]
pub enum CrfError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid model: {0}")]
    InvalidModel(&'static str),
    #[error("unsupported GCRF version {0}")]
    UnsupportedVersion(u32),
    #[error("GCRF checksum mismatch")]
    ChecksumMismatch,
    #[error("invalid floating-point value: {0:?}")]
    InvalidFloat(String),
    #[error("invalid pattern: {0:?}")]
    InvalidPattern(String),
    #[error("row {row} has no column {column}")]
    MissingColumn { row: usize, column: usize },
    #[error("observation at row {row}, column {column} must start with u, b, or *")]
    InvalidObservation { row: usize, column: usize },
    #[error("model or sequence exceeds supported dimensions")]
    TooLarge,
}

/// Model type as stored by Wapiti.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ModelType {
    MaxEnt = 0,
    Memm = 1,
    Crf = 2,
}

impl TryFrom<u8> for ModelType {
    type Error = CrfError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::MaxEnt),
            1 => Ok(Self::Memm),
            2 => Ok(Self::Crf),
            _ => Err(CrfError::InvalidModel("unknown model type")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnigramWeight {
    pub label: u16,
    pub weight: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BigramWeight {
    pub prev: u16,
    pub cur: u16,
    pub weight: f64,
}

/// Counts for the compact, in-memory model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelStats {
    pub num_labels: usize,
    pub num_patterns: usize,
    pub num_observations: usize,
    pub num_unigram_weights: usize,
    pub num_bigram_weights: usize,
    pub observation_bytes: usize,
    pub hash_capacity: usize,
}

#[derive(Debug)]
struct Sparse<T> {
    offsets: Vec<u32>,
    weights: Vec<T>,
}

impl<T> Default for Sparse<T> {
    fn default() -> Self {
        Self {
            offsets: vec![0],
            weights: Vec::new(),
        }
    }
}

impl<T> Sparse<T> {
    fn get(&self, id: usize) -> &[T] {
        &self.weights[self.offsets[id] as usize..self.offsets[id + 1] as usize]
    }

    fn end_row(&mut self) -> Result<(), CrfError> {
        self.offsets.push(
            self.weights
                .len()
                .try_into()
                .map_err(|_| CrfError::TooLarge)?,
        );
        Ok(())
    }
}

#[derive(Debug)]
struct Observations {
    bytes: Vec<u8>,
    offsets: Vec<u32>,
    // Zero is an empty bucket. Occupied buckets store ID + 1.
    table: Vec<u32>,
}

impl Default for Observations {
    fn default() -> Self {
        Self {
            bytes: Vec::new(),
            offsets: vec![0],
            table: Vec::new(),
        }
    }
}

impl Observations {
    fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    fn get(&self, id: usize) -> &[u8] {
        &self.bytes[self.offsets[id] as usize..self.offsets[id + 1] as usize]
    }

    fn push(&mut self, bytes: &[u8]) -> Result<(), CrfError> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(CrfError::TooLarge)?;
        self.offsets
            .push(end.try_into().map_err(|_| CrfError::TooLarge)?);
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn capacity_for(count: usize) -> Result<usize, CrfError> {
        count
            .checked_mul(2)
            .and_then(usize::checked_next_power_of_two)
            .ok_or(CrfError::TooLarge)
    }

    fn build_table(&mut self) -> Result<(), CrfError> {
        self.table = vec![0; Self::capacity_for(self.len())?];
        for id in 0..self.len() {
            let (slot, existing) = self.probe(self.get(id));
            if existing.is_some() {
                return Err(CrfError::InvalidModel("duplicate active observation"));
            }
            self.table[slot] = u32::try_from(id + 1).map_err(|_| CrfError::TooLarge)?;
        }
        Ok(())
    }

    fn probe(&self, key: &[u8]) -> (usize, Option<usize>) {
        let mask = self.table.len() - 1;
        let mut slot = xxh3_64(key) as usize & mask;
        loop {
            let entry = self.table[slot];
            if entry == 0 {
                return (slot, None);
            }
            let id = (entry - 1) as usize;
            if self.get(id) == key {
                return (slot, Some(id));
            }
            slot = (slot + 1) & mask;
        }
    }

    fn find(&self, key: &[u8]) -> Option<usize> {
        self.probe(key).1
    }
}

/// An immutable model. Share it across threads and give each thread its own
/// [DecodeScratch].
#[derive(Debug)]
pub struct CrfModel {
    model_type: ModelType,
    autouni: bool,
    labels: Vec<String>,
    patterns: Vec<Pattern>,
    observations: Observations,
    unigram: Sparse<UnigramWeight>,
    bigram: Sparse<BigramWeight>,
}

impl CrfModel {
    /// Load a compact binary model, a gzip-compressed Wapiti model, or a
    /// plain Wapiti model by inspecting the file's magic bytes.
    pub fn load_auto(path: impl AsRef<Path>) -> Result<Self, CrfError> {
        wapiti::load_path(path.as_ref(), true)
    }

    /// Return the label with the given ID. Panics if the ID is out of bounds.
    pub fn label_name(&self, id: usize) -> &str {
        &self.labels[id]
    }

    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    pub fn num_labels(&self) -> usize {
        self.labels.len()
    }

    pub fn model_type(&self) -> ModelType {
        self.model_type
    }

    pub fn stats(&self) -> ModelStats {
        ModelStats {
            num_labels: self.num_labels(),
            num_patterns: self.patterns.len(),
            num_observations: self.observations.len(),
            num_unigram_weights: self.unigram.weights.len(),
            num_bigram_weights: self.bigram.weights.len(),
            observation_bytes: self.observations.bytes.len(),
            hash_capacity: self.observations.table.len(),
        }
    }
}

fn is_c_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t'..=b'\r')
}
