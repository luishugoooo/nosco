//! Scholarly document models and serializers for JSON and GROBID-style TEI P5.

mod model;
mod tei;

pub use model::*;
pub use tei::{
    TeiOptions, citation_to_tei_xml, header_to_tei_xml, references_to_tei_xml, to_tei_xml,
};

/// An error while serializing a scholarly document.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("could not serialize JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("could not write TEI XML: {0}")]
    Xml(#[from] std::io::Error),
    #[error("TEI XML output was not UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("U+{0:04X} is not allowed in XML 1.0")]
    InvalidXmlCharacter(u32),
}

macro_rules! serialization_methods {
    ($model:ty, $tei:path) => {
        impl $model {
            /// Serializes the model to compact JSON.
            pub fn to_json(&self) -> Result<String, ModelError> {
                Ok(serde_json::to_string(self)?)
            }

            /// Serializes the model to indented JSON.
            pub fn to_json_pretty(&self) -> Result<String, ModelError> {
                Ok(serde_json::to_string_pretty(self)?)
            }

            /// Serializes the model to TEI P5 XML.
            pub fn to_tei_xml(&self, opts: &TeiOptions) -> Result<String, ModelError> {
                $tei(self, opts)
            }
        }
    };
}

serialization_methods!(Document, to_tei_xml);
serialization_methods!(Header, header_to_tei_xml);
serialization_methods!(BibItem, citation_to_tei_xml);

#[cfg(test)]
mod tests;
