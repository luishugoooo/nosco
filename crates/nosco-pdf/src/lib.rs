//! Extract text, font information, and page geometry from PDF documents.
#![forbid(unsafe_code)]

mod cff;
mod cmap;
mod fonts;
mod interpreter;
mod layout;
mod metadata;
mod truetype;

use std::{collections::BTreeMap, ops::Range, path::Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors loading a PDF or interpreting its page content.
#[derive(Debug, Clone, Serialize, Deserialize, Error)]
pub enum PdfError {
    #[error("could not read PDF file: {0}")]
    Io(String),
    #[error("could not load PDF: {0}")]
    Parse(String),
    #[error("could not unlock PDF with an empty password: {0}")]
    Encrypted(String),
    #[error("invalid PDF page {page}: {message}")]
    Page { page: usize, message: String },
}

/// A rectangle in page points with a top-left origin and a 1-based page number.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoundingBox {
    pub page: usize,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl BoundingBox {
    pub fn new(page: usize, x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            page,
            x,
            y,
            width,
            height,
        }
    }

    pub fn x2(&self) -> f64 {
        self.x + self.width
    }

    pub fn y2(&self) -> f64 {
        self.y + self.height
    }

    pub fn area(&self) -> f64 {
        self.width * self.height
    }

    pub fn intersects(&self, other: &Self) -> bool {
        self.intersection(other, false).is_some()
    }

    /// Return the enclosing rectangle.
    ///
    /// # Panics
    /// Panics if the boxes belong to different pages.
    pub fn union(&self, other: &Self) -> Self {
        assert_eq!(
            self.page, other.page,
            "cannot combine boxes from different pages"
        );
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self::new(
            self.page,
            x,
            y,
            self.x2().max(other.x2()) - x,
            self.y2().max(other.y2()) - y,
        )
    }

    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        self.x <= x && x <= self.x2() && self.y <= y && y <= self.y2()
    }

    fn intersection(&self, other: &Self, allow_edge: bool) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let width = self.x2().min(other.x2()) - x;
        let height = self.y2().min(other.y2()) - y;
        (self.page == other.page
            && ((width > 0.0 && height > 0.0) || (allow_edge && width >= 0.0 && height >= 0.0)))
            .then(|| Self::new(self.page, x, y, width, height))
    }
}

/// A word fragment or punctuation mark in reading order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutToken {
    pub text: String,
    pub page: usize,
    pub bbox: BoundingBox,
    pub font_name: String,
    pub font_size: f64,
    pub bold: bool,
    pub italic: bool,
    pub subscript: bool,
    pub superscript: bool,
    pub color: (u8, u8, u8),
    pub rotated: bool,
    pub space_after: bool,
    pub new_line_after: bool,
    pub block_idx: usize,
    pub line_idx: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutLine {
    pub page: usize,
    pub bbox: BoundingBox,
    pub token_range: Range<usize>,
    pub block_idx: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutBlock {
    pub page: usize,
    pub bbox: BoundingBox,
    pub token_range: Range<usize>,
    pub line_range: Range<usize>,
    pub block_idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphicKind {
    Image,
    VectorBox,
    HorizontalRule,
    VerticalRule,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphicObject {
    pub page: usize,
    pub bbox: BoundingBox,
    pub kind: GraphicKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfLink {
    pub page: usize,
    pub bbox: BoundingBox,
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfOutline {
    pub title: String,
    pub page: Option<usize>,
    pub level: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PdfMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub doi: Option<String>,
    pub isbn: Option<String>,
    pub issn: Option<String>,
    pub arxiv: Option<String>,
    pub creation_date: Option<String>,
    pub modified_date: Option<String>,
    pub xmp: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageInfo {
    pub number: usize,
    pub width: f64,
    pub height: f64,
    pub main_area: BoundingBox,
    pub block_range: Range<usize>,
    pub line_range: Range<usize>,
    pub token_range: Range<usize>,
}

/// Page numbers are 1-based. Ranges and indices refer to zero-based document vectors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LayoutDocument {
    pub pages: Vec<PageInfo>,
    pub blocks: Vec<LayoutBlock>,
    pub lines: Vec<LayoutLine>,
    pub tokens: Vec<LayoutToken>,
    pub graphics: Vec<GraphicObject>,
    #[serde(default)]
    pub links: Vec<PdfLink>,
    #[serde(default)]
    pub outlines: Vec<PdfOutline>,
    pub metadata: PdfMetadata,
}

impl LayoutDocument {
    pub fn block_tokens(&self, block_idx: usize) -> &[LayoutToken] {
        self.blocks
            .get(block_idx)
            .and_then(|block| self.tokens.get(block.token_range.clone()))
            .unwrap_or_default()
    }

    pub fn line_tokens(&self, line_idx: usize) -> &[LayoutToken] {
        self.lines
            .get(line_idx)
            .and_then(|line| self.tokens.get(line.token_range.clone()))
            .unwrap_or_default()
    }

    pub fn block_text(&self, block_idx: usize) -> String {
        tokens_text(self.block_tokens(block_idx))
    }

    /// Return the text of the page at a zero-based index.
    pub fn page_text(&self, page_idx: usize) -> String {
        let tokens = self
            .pages
            .get(page_idx)
            .and_then(|page| self.tokens.get(page.token_range.clone()));
        tokens_text(tokens.unwrap_or_default())
    }

    pub fn full_text(&self) -> String {
        tokens_text(&self.tokens)
    }

    /// Return the most common font size to the nearest tenth of a point, weighted by character count.
    pub fn dominant_font_size(&self) -> f64 {
        let mut sizes = BTreeMap::new();
        for token in &self.tokens {
            if token.font_size.is_finite() && token.font_size > 0.0 {
                *sizes
                    .entry((token.font_size * 10.0).round() as u64)
                    .or_insert(0usize) += token.text.chars().count();
            }
        }
        sizes
            .into_iter()
            .max_by_key(|&(size, count)| (count, size))
            .map_or(0.0, |(size, _)| size as f64 / 10.0)
    }
}

fn tokens_text(tokens: &[LayoutToken]) -> String {
    let mut text = String::new();
    for (index, token) in tokens.iter().enumerate() {
        text.push_str(&token.text);
        if index + 1 < tokens.len() {
            if token.new_line_after {
                text.push('\n');
            } else if token.space_after {
                text.push(' ');
            }
        }
    }
    text
}

/// Limits for extraction. Page numbers always refer to the original PDF.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtractOptions {
    pub max_pages: Option<usize>,
}

pub fn extract_from_bytes(bytes: &[u8]) -> Result<LayoutDocument, PdfError> {
    extract_from_bytes_with_options(bytes, ExtractOptions::default())
}

pub fn extract_from_bytes_with_options(
    bytes: &[u8],
    options: ExtractOptions,
) -> Result<LayoutDocument, PdfError> {
    let mut pdf = lopdf::Document::load_mem_with_options(
        bytes,
        lopdf::LoadOptions::with_filter(strip_raster),
    )
    .map_err(|error| PdfError::Parse(error.to_string()))?;
    if pdf.page_iter().next().is_none() {
        pdf =
            lopdf::Document::load_mem(bytes).map_err(|error| PdfError::Parse(error.to_string()))?;
    }
    extract(pdf, options)
}

pub fn extract_from_file(path: &Path) -> Result<LayoutDocument, PdfError> {
    extract_from_file_with_options(path, ExtractOptions::default())
}

pub fn extract_from_file_with_options(
    path: &Path,
    options: ExtractOptions,
) -> Result<LayoutDocument, PdfError> {
    let mut pdf =
        lopdf::Document::load_with_options(path, lopdf::LoadOptions::with_filter(strip_raster))
            .map_err(|error| match error {
                lopdf::Error::IO(error) => PdfError::Io(error.to_string()),
                error => PdfError::Parse(error.to_string()),
            })?;
    if pdf.page_iter().next().is_none() {
        pdf = lopdf::Document::load(path).map_err(|error| PdfError::Parse(error.to_string()))?;
    }
    extract(pdf, options)
}

fn strip_raster(
    id: lopdf::ObjectId,
    object: &mut lopdf::Object,
) -> Option<(lopdf::ObjectId, lopdf::Object)> {
    if let Ok(stream) = object.as_stream_mut() {
        discard_image_bytes(stream);
        // lopdf uses the in-place object for xref entries. Objects contained
        // in object streams use the returned value and cannot be streams.
        Some((id, lopdf::Object::Null))
    } else {
        Some((id, object.clone()))
    }
}

fn discard_image_bytes(stream: &mut lopdf::Stream) {
    if stream
        .dict
        .get(b"Subtype")
        .and_then(lopdf::Object::as_name)
        .ok()
        == Some(b"Image")
    {
        stream.content = Vec::new();
        // An empty stream with a saved offset would be read again by lopdf.
        stream.start_position = None;
    }
}

fn extract(mut pdf: lopdf::Document, options: ExtractOptions) -> Result<LayoutDocument, PdfError> {
    if pdf.is_encrypted() {
        pdf.decrypt("")
            .map_err(|error| PdfError::Encrypted(error.to_string()))?;
    }
    // The encrypted and recovery paths may bypass the object filter.
    for object in pdf.objects.values_mut() {
        if let Ok(stream) = object.as_stream_mut() {
            discard_image_bytes(stream);
        }
    }
    interpreter::extract(&pdf, options)
}
