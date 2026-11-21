use std::{fmt, fs, path::{Path, PathBuf}};

use nosco_crf::{CrfModel, DecodeScratch};
use nosco_features::FeatureMatrix;
use nosco_lexicon::Lexicon;
use rayon::prelude::*;

use crate::EngineError;

const LEXICON_FILE: &str = "lexicon.glex";

/// The standard GROBID Wapiti models used by the cascade.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(usize)]
pub enum ModelKind {
    Date,
    NameHeader,
    NameCitation,
    AffiliationAddress,
    Citation,
    ReferenceSegmenter,
    Header,
    Segmentation,
    Fulltext,
    Figure,
    Table,
}

impl ModelKind {
    pub const ALL: [Self; 11] = [
        Self::Date, Self::NameHeader, Self::NameCitation, Self::AffiliationAddress,
        Self::Citation, Self::ReferenceSegmenter, Self::Header, Self::Segmentation,
        Self::Fulltext, Self::Figure, Self::Table,
    ];

    pub const fn slug(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::NameHeader => "name-header",
            Self::NameCitation => "name-citation",
            Self::AffiliationAddress => "affiliation-address",
            Self::Citation => "citation",
            Self::ReferenceSegmenter => "reference-segmenter",
            Self::Header => "header",
            Self::Segmentation => "segmentation",
            Self::Fulltext => "fulltext",
            Self::Figure => "figure",
            Self::Table => "table",
        }
    }

    pub fn wapiti_path(self) -> PathBuf {
        let subpath = match self {
            Self::NameHeader => "name/header",
            Self::NameCitation => "name/citation",
            other => other.slug(),
        };
        Path::new("models").join(subpath).join("model.wapiti")
    }

    pub fn compact_path(self) -> PathBuf {
        PathBuf::from(format!("{}.gcrf", self.slug()))
    }
}

impl fmt::Display for ModelKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

#[derive(Debug, Clone)]
pub struct ModelConversionStat {
    pub kind: ModelKind,
    pub input_bytes: u64,
    pub output_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ConversionReport {
    pub models: Vec<ModelConversionStat>,
    /// Total Wapiti model input bytes.
    pub total_input_bytes: u64,
    /// Total output bytes, including the lexicon.
    pub total_output_bytes: u64,
    pub lexicon_output_bytes: u64,
}

/// Builds a compact pack in the requested output directory.
pub fn convert_grobid_home(
    grobid_home: &Path,
    output_dir: &Path,
) -> Result<ConversionReport, EngineError> {
    fs::create_dir_all(output_dir).map_err(|source| EngineError::Io {
        path: output_dir.to_owned(), source,
    })?;
    let (models, lexicon_bytes) = rayon::join(
        || {
            ModelKind::ALL.par_iter().map(|&kind| {
                let input = grobid_home.join(kind.wapiti_path());
                let output = output_dir.join(kind.compact_path());
                let input_bytes = file_size(&input)?;
                let model = load_model(kind, &input)?;
                model.save_gcrf(&output).map_err(|source| EngineError::Model {
                    kind, path: output.clone(), source,
                })?;
                Ok(ModelConversionStat { kind, input_bytes, output_bytes: file_size(&output)? })
            }).collect::<Result<Vec<_>, EngineError>>()
        },
        || {
            let output = output_dir.join(LEXICON_FILE);
            Lexicon::load_from_grobid_home(grobid_home)?.save_glex(&output)?;
            file_size(&output)
        },
    );
    let models = models?;
    let lexicon_output_bytes = lexicon_bytes?;
    Ok(ConversionReport {
        total_input_bytes: models.iter().map(|stat| stat.input_bytes).sum(),
        total_output_bytes: lexicon_output_bytes + models.iter().map(|stat| stat.output_bytes).sum::<u64>(),
        models,
        lexicon_output_bytes,
    })
}

fn file_size(path: &Path) -> Result<u64, EngineError> {
    fs::metadata(path).map(|meta| meta.len()).map_err(|source| EngineError::Io {
        path: path.to_owned(), source,
    })
}

fn load_model(kind: ModelKind, path: &Path) -> Result<CrfModel, EngineError> {
    CrfModel::load_auto(path).map_err(|source| EngineError::Model {
        kind, path: path.to_owned(), source,
    })
}

/// Immutable models and lexicon. Calls may share an engine across threads.
pub struct Engine {
    models: Vec<Option<CrfModel>>,
    lexicon: Lexicon,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("models", &self.models.iter().flatten().count())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Opens a compact pack or a raw GROBID home.
    pub fn load(assets_dir: &Path) -> Result<Self, EngineError> {
        let compact = assets_dir.join(LEXICON_FILE).exists()
            || ModelKind::ALL
                .iter()
                .any(|kind| assets_dir.join(kind.compact_path()).exists());
        Self::load_assets(assets_dir, compact, false)
    }

    pub fn load_from_grobid_home(grobid_home: &Path) -> Result<Self, EngineError> {
        Self::load_assets(grobid_home, false, false)
    }

    /// Loads the models required to extract headers. Venue parsing is optional.
    pub fn load_for_header(assets_dir: &Path) -> Result<Self, EngineError> {
        let compact = assets_dir.join(LEXICON_FILE).exists()
            || assets_dir.join(ModelKind::Header.compact_path()).exists();
        Self::load_assets(assets_dir, compact, true)
    }

    fn load_assets(dir: &Path, compact: bool, header_only: bool) -> Result<Self, EngineError> {
        let (models, lexicon) = rayon::join(
            || {
                ModelKind::ALL
                    .par_iter()
                    .map(|&kind| {
                        let relative = if compact {
                            kind.compact_path()
                        } else {
                            kind.wapiti_path()
                        };
                        let path = dir.join(relative);
                        if header_only
                            && matches!(
                                kind,
                                ModelKind::ReferenceSegmenter
                                    | ModelKind::Fulltext
                                    | ModelKind::Figure
                                    | ModelKind::Table
                            )
                        {
                            return Ok(None);
                        }
                        if header_only
                            && matches!(kind, ModelKind::Citation | ModelKind::NameCitation)
                            && !path.exists()
                        {
                            return Ok(None);
                        }
                        load_model(kind, &path).map(Some)
                    })
                    .collect::<Result<Vec<_>, _>>()
            },
            || if compact {
                Lexicon::load_glex(&dir.join(LEXICON_FILE))
            } else {
                Lexicon::load_from_grobid_home(dir)
            },
        );
        Ok(Self { models: models?, lexicon: lexicon? })
    }

    pub fn lexicon(&self) -> &Lexicon {
        &self.lexicon
    }

    pub fn model(&self, kind: ModelKind) -> &CrfModel {
        self.model_if_loaded(kind)
            .expect("requested model was not loaded")
    }

    pub(crate) fn model_if_loaded(&self, kind: ModelKind) -> Option<&CrfModel> {
        self.models[kind as usize].as_ref()
    }

    pub(crate) fn tag(
        &self,
        kind: ModelKind,
        features: &FeatureMatrix,
        scratch: &mut DecodeScratch,
    ) -> Result<Vec<&str>, EngineError> {
        let model = self
            .model_if_loaded(kind)
            .ok_or(EngineError::MissingModel(kind))?;
        let ids = model
            .tag(features.as_rows(), scratch)
            .map_err(|source| EngineError::Decode { kind, source })?;
        Ok(ids.into_iter().map(|id| model.label_name(id)).collect())
    }
}
