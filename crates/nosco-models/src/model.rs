use serde::{Deserialize, Serialize};

fn is_false(value: &bool) -> bool {
    !value
}

/// A bounding box in PDF page coordinates. GROBID page numbers are one-based.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BBox {
    pub page: usize,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// An extracted date with optional normalized components and its original text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Date {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<u8>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub raw: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Person {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub middle_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub affiliation_markers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub affiliation_keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orcid: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub corresp: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Address {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addr_line: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_box: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settlement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Affiliation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub institutions: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub departments: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub laboratories: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<Address>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_affiliation: Option<String>,
}

/// Bibliographic and descriptive metadata for a document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Header {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<Person>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub affiliations: Vec<Affiliation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abstract_text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dates: Vec<Date>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication_date: Option<Date>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doi: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arxiv_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pmid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pmcid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eissn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meeting: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copyright: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub funding_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submission_note: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
}

/// A bibliographic reference.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BibItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<Person>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub editors: Vec<Person>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_page: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_page: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_range: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<Date>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doi: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arxiv_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pmid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pmcid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isbn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub raw_citation: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bboxes: Vec<BBox>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalloutKind {
    #[default]
    Bibr,
    Figure,
    Table,
    Formula,
    Footnote,
}

/// An inline reference within a paragraph.
///
/// Offsets are end-exclusive UTF-8 byte offsets. Unicode character offsets are
/// also accepted when they reproduce the supplied callout text.
/// TEI serialization skips invalid spans and resolves overlaps by earliest
/// start, then longest span.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Callout {
    pub kind: CalloutKind,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    pub char_start: usize,
    pub char_end: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Paragraph {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub callouts: Vec<Callout>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bboxes: Vec<BBox>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Formula {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bboxes: Vec<BBox>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BodyElement {
    Paragraph(Paragraph),
    Formula(Formula),
    List(Vec<ListItem>),
    Note(String),
}

impl Default for BodyElement {
    fn default() -> Self {
        Self::Paragraph(Paragraph::default())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Section {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<BodyElement>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subsections: Vec<Section>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Figure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bboxes: Vec<BBox>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableCell {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    pub cols: usize,
    pub rows: usize,
    #[serde(skip_serializing_if = "is_false")]
    pub is_header: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TableRow {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cells: Vec<TableCell>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Table {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<TableRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bboxes: Vec<BBox>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Footnote {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
}

/// A structured scholarly document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Document {
    pub header: Header,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<Section>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub figures: Vec<Figure>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<Table>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<BibItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub footnotes: Vec<Footnote>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledgements: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub annexes: Vec<Section>,
    pub page_count: usize,
}

impl BBox {
    /// Formats the coordinates with GROBID's two decimal places.
    pub fn to_coords_string(&self) -> String {
        format!(
            "{},{:.2},{:.2},{:.2},{:.2}",
            self.page, self.x, self.y, self.width, self.height
        )
    }
}

impl Date {
    /// Formats the longest valid date prefix, starting with a four-digit year.
    pub fn to_iso8601(&self) -> Option<String> {
        use std::fmt::Write as _;

        let year = self.year.filter(|year| (0..=9999).contains(year))?;
        let mut date = format!("{year:04}");
        if let Some(month) = self.month.filter(|month| (1..=12).contains(month)) {
            let _ = write!(date, "-{month:02}");
            let days = match month {
                2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
                2 => 28,
                4 | 6 | 9 | 11 => 30,
                _ => 31,
            };
            if let Some(day) = self.day.filter(|day| (1..=days).contains(day)) {
                let _ = write!(date, "-{day:02}");
            }
        }
        Some(date)
    }

    pub fn is_empty(&self) -> bool {
        self.year.is_none()
            && self.month.is_none()
            && self.day.is_none()
            && self.raw.trim().is_empty()
    }
}

impl Person {
    pub fn is_empty(&self) -> bool {
        self.name_parts()
            .into_iter()
            .all(|part| nonblank(part).is_none())
            && nonblank(self.raw_name.as_deref()).is_none()
            && nonblank(self.email.as_deref()).is_none()
            && nonblank(self.orcid.as_deref()).is_none()
            && self.affiliation_markers.is_empty()
            && self.affiliation_keys.is_empty()
            && !self.corresp
    }

    /// Joins the name components in display order, with a raw name fallback.
    pub fn full_name(&self) -> String {
        let name = self
            .name_parts()
            .into_iter()
            .filter_map(nonblank)
            .map(str::trim)
            .collect::<Vec<_>>();
        if name.is_empty() {
            self.raw_name
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_owned()
        } else {
            name.join(" ")
        }
    }

    pub(crate) fn name_parts(&self) -> [Option<&str>; 5] {
        [
            self.title.as_deref(),
            self.first_name.as_deref(),
            self.middle_name.as_deref(),
            self.last_name.as_deref(),
            self.suffix.as_deref(),
        ]
    }
}

impl Address {
    pub fn is_empty(&self) -> bool {
        [
            &self.addr_line,
            &self.post_code,
            &self.post_box,
            &self.settlement,
            &self.region,
            &self.country,
            &self.country_code,
        ]
        .into_iter()
        .all(|field| nonblank(field.as_deref()).is_none())
    }
}

impl Affiliation {
    pub fn is_empty(&self) -> bool {
        [&self.key, &self.marker, &self.raw_affiliation]
            .into_iter()
            .all(|field| nonblank(field.as_deref()).is_none())
            && self
                .institutions
                .iter()
                .chain(&self.departments)
                .chain(&self.laboratories)
                .all(|name| name.trim().is_empty())
            && self.address.as_ref().is_none_or(Address::is_empty)
    }
}

pub(crate) fn nonblank(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}
