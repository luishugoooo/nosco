//! Pure-Rust GROBID cascade for scholarly PDFs and bibliographic strings.

mod assets;
mod callouts;
mod document;
mod fulltext;
mod identifiers;
mod metadata;
mod spans;
mod text;

use std::io;
use std::path::PathBuf;

pub use assets::{ConversionReport, Engine, ModelConversionStat, ModelKind, convert_grobid_home};
use nosco_crf::CrfError;
use nosco_lexicon::LexiconError;
use nosco_pdf::PdfError;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0} model was not loaded")]
    MissingModel(ModelKind),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{kind} model at {path}: {source}")]
    Model {
        kind: ModelKind,
        path: PathBuf,
        #[source]
        source: CrfError,
    },
    #[error("{kind} decoding failed: {source}")]
    Decode {
        kind: ModelKind,
        #[source]
        source: CrfError,
    },
    #[error(transparent)]
    Lexicon(#[from] LexiconError),
    #[error(transparent)]
    Pdf(#[from] PdfError),
}
