use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};

use crate::model::nonblank;
use crate::{
    BBox, BibItem, BodyElement, Callout, CalloutKind, Document, Figure, Footnote, Formula, Header,
    ModelError, Paragraph, Section, Table,
};

mod bibliography;

const TEI_NS: (&str, &str) = ("xmlns", "http://www.tei-c.org/ns/1.0");
type XmlResult = Result<(), ModelError>;

/// Controls the formatting and optional fields of TEI output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TeiOptions {
    /// Indent block elements while preserving inline text.
    pub pretty: bool,
    /// Emit available PDF bounding boxes as `coords` attributes.
    pub include_coordinates: bool,
    /// Include the original citation text in a `raw_reference` note.
    pub include_raw_citations: bool,
}

/// Serializes a document as a standalone TEI P5 document.
pub fn to_tei_xml(doc: &Document, opts: &TeiOptions) -> Result<String, ModelError> {
    let mut xml = TeiWriter::new(opts);
    xml.writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;
    xml.element("TEI", &[TEI_NS], |xml| {
        xml.header(&doc.header, doc.page_count, &[])?;
        xml.element("text", &[], |xml| {
            xml.element("body", &[], |xml| {
                for section in &doc.body {
                    xml.section(section, &[])?;
                }
                for figure in &doc.figures {
                    xml.figure(figure)?;
                }
                for table in &doc.tables {
                    xml.table(table)?;
                }
                for footnote in &doc.footnotes {
                    xml.footnote(footnote)?;
                }
                Ok(())
            })?;
            xml.back(doc)
        })
    })?;
    xml.finish()
}

/// Serializes a standalone, namespaced `teiHeader` element.
pub fn header_to_tei_xml(header: &Header, opts: &TeiOptions) -> Result<String, ModelError> {
    let mut xml = TeiWriter::new(opts);
    xml.header(header, 0, &[TEI_NS])?;
    xml.finish()
}

/// Serializes a standalone, namespaced `listBibl` element.
pub fn references_to_tei_xml(refs: &[BibItem], opts: &TeiOptions) -> Result<String, ModelError> {
    let mut xml = TeiWriter::new(opts);
    xml.references(refs, &[TEI_NS])?;
    xml.finish()
}

/// Serializes a standalone, namespaced `biblStruct` element.
pub fn citation_to_tei_xml(item: &BibItem, opts: &TeiOptions) -> Result<String, ModelError> {
    let mut xml = TeiWriter::new(opts);
    xml.citation(item, "", &[TEI_NS])?;
    xml.finish()
}

struct TeiWriter<'a> {
    writer: Writer<Vec<u8>>,
    opts: &'a TeiOptions,
}

impl<'a> TeiWriter<'a> {
    fn new(opts: &'a TeiOptions) -> Self {
        let writer = if opts.pretty {
            Writer::new_with_indent(Vec::new(), b' ', 2)
        } else {
            Writer::new(Vec::new())
        };
        Self { writer, opts }
    }

    fn finish(self) -> Result<String, ModelError> {
        Ok(String::from_utf8(self.writer.into_inner())?)
    }

    fn tag<'n>(name: &'n str, attrs: &[(&str, &str)]) -> Result<BytesStart<'n>, ModelError> {
        let mut tag = BytesStart::new(name);
        for &(key, value) in attrs {
            if !value.is_empty() {
                validate_xml(value)?;
                tag.push_attribute((key, value));
            }
        }
        Ok(tag)
    }

    fn element(
        &mut self,
        name: &str,
        attrs: &[(&str, &str)],
        content: impl FnOnce(&mut Self) -> XmlResult,
    ) -> XmlResult {
        self.writer
            .write_event(Event::Start(Self::tag(name, attrs)?))?;
        content(self)?;
        self.writer.write_event(Event::End(BytesEnd::new(name)))?;
        Ok(())
    }

    fn empty(&mut self, name: &str, attrs: &[(&str, &str)]) -> XmlResult {
        self.writer
            .write_event(Event::Empty(Self::tag(name, attrs)?))?;
        Ok(())
    }

    fn text(&mut self, value: &str) -> XmlResult {
        validate_xml(value)?;
        self.writer
            .write_event(Event::Text(BytesText::new(value)))?;
        Ok(())
    }

    fn leaf(&mut self, name: &str, value: Option<&str>, attrs: &[(&str, &str)]) -> XmlResult {
        if let Some(value) = nonblank(value) {
            self.element(name, attrs, |xml| xml.text(value))?;
        }
        Ok(())
    }

    fn coords(&self, boxes: &[BBox]) -> String {
        if self.opts.include_coordinates {
            boxes
                .iter()
                .map(BBox::to_coords_string)
                .collect::<Vec<_>>()
                .join(";")
        } else {
            String::new()
        }
    }

    fn section(&mut self, section: &Section, attrs: &[(&str, &str)]) -> XmlResult {
        self.element("div", attrs, |xml| {
            let number = section.number.as_deref().unwrap_or_default();
            if let Some(title) = nonblank(section.title.as_deref()) {
                xml.leaf("head", Some(title), &[("n", number)])?;
            } else if !number.is_empty() {
                xml.empty("head", &[("n", number)])?;
            }
            for element in &section.elements {
                match element {
                    BodyElement::Paragraph(paragraph) => xml.paragraph(paragraph)?,
                    BodyElement::Formula(formula) => xml.formula(formula)?,
                    BodyElement::List(items) => {
                        xml.element("list", &[], |xml| {
                            for item in items {
                                let label = item.label.as_deref().unwrap_or_default();
                                xml.element("item", &[("n", label)], |xml| xml.text(&item.text))?;
                            }
                            Ok(())
                        })?;
                    }
                    BodyElement::Note(text) => xml.leaf("note", Some(text), &[])?,
                }
            }
            for subsection in &section.subsections {
                xml.section(subsection, &[])?;
            }
            Ok(())
        })
    }

    fn paragraph(&mut self, paragraph: &Paragraph) -> XmlResult {
        let coords = self.coords(&paragraph.bboxes);
        self.element("p", &[("coords", &coords)], |xml| {
            let text = &paragraph.text;
            let mut cursor = 0;
            for (start, end, callout) in callout_spans(paragraph) {
                if start < cursor {
                    continue;
                }
                // Even an empty text event suppresses indentation before an inline ref.
                xml.text(&text[cursor..start])?;
                let kind = match callout.kind {
                    CalloutKind::Bibr => "bibr",
                    CalloutKind::Figure => "figure",
                    CalloutKind::Table => "table",
                    CalloutKind::Formula => "formula",
                    CalloutKind::Footnote => "foot",
                };
                let target = callout
                    .target_id
                    .as_deref()
                    .map(|id| id.trim().trim_start_matches('#'))
                    .filter(|id| !id.is_empty())
                    .map(|id| format!("#{id}"))
                    .unwrap_or_default();
                xml.element("ref", &[("type", kind), ("target", &target)], |xml| {
                    xml.text(&text[start..end])
                })?;
                cursor = end;
            }
            xml.text(&text[cursor..])
        })
    }

    fn formula(&mut self, formula: &Formula) -> XmlResult {
        let coords = self.coords(&formula.bboxes);
        let id = formula.id.as_deref().unwrap_or_default();
        self.element("formula", &[("xml:id", id), ("coords", &coords)], |xml| {
            xml.text(&formula.text)?;
            xml.leaf("label", formula.label.as_deref(), &[])?;
            xml.text("")
        })
    }

    fn figure(&mut self, figure: &Figure) -> XmlResult {
        let id = figure.id.as_deref().unwrap_or_default();
        let coords = self.coords(&figure.bboxes);
        self.element("figure", &[("xml:id", id), ("coords", &coords)], |xml| {
            xml.leaf("head", figure.header.as_deref(), &[])?;
            xml.leaf("label", figure.label.as_deref(), &[])?;
            xml.leaf("figDesc", figure.caption.as_deref(), &[])?;
            xml.leaf(
                "note",
                figure.content.as_deref(),
                &[("type", "figure_content")],
            )
        })
    }

    fn table(&mut self, table: &Table) -> XmlResult {
        let id = table.id.as_deref().unwrap_or_default();
        let coords = self.coords(&table.bboxes);
        let attrs = [("type", "table"), ("xml:id", id), ("coords", &coords)];
        self.element("figure", &attrs, |xml| {
            xml.leaf("head", table.header.as_deref(), &[])?;
            xml.leaf("label", table.label.as_deref(), &[])?;
            xml.leaf("figDesc", table.caption.as_deref(), &[])?;
            if !table.rows.is_empty() {
                xml.element("table", &[], |xml| {
                    for row in &table.rows {
                        xml.element("row", &[], |xml| {
                            for cell in &row.cells {
                                let cols = (cell.cols > 1).then(|| cell.cols.to_string());
                                let rows = (cell.rows > 1).then(|| cell.rows.to_string());
                                let attrs = [
                                    ("cols", cols.as_deref().unwrap_or_default()),
                                    ("rows", rows.as_deref().unwrap_or_default()),
                                    ("role", if cell.is_header { "label" } else { "" }),
                                ];
                                xml.element("cell", &attrs, |xml| xml.text(&cell.text))?;
                            }
                            Ok(())
                        })?;
                    }
                    Ok(())
                })?;
            }
            xml.leaf(
                "note",
                table.content.as_deref(),
                &[("type", "table_content")],
            )?;
            xml.leaf("note", table.note.as_deref(), &[])
        })
    }

    fn footnote(&mut self, footnote: &Footnote) -> XmlResult {
        let id = footnote.id.as_deref().unwrap_or_default();
        let label = footnote.label.as_deref().unwrap_or_default();
        self.element(
            "note",
            &[("place", "foot"), ("xml:id", id), ("n", label)],
            |xml| {
                if let Some(page) = footnote.page {
                    xml.text("")?;
                    xml.empty("pb", &[("n", &page.to_string())])?;
                }
                xml.text(&footnote.text)
            },
        )
    }

    fn back(&mut self, doc: &Document) -> XmlResult {
        let acknowledgement = nonblank(doc.acknowledgements.as_deref());
        if acknowledgement.is_none() && doc.annexes.is_empty() && doc.references.is_empty() {
            return Ok(());
        }
        self.element("back", &[], |xml| {
            if let Some(text) = acknowledgement {
                xml.element("div", &[("type", "acknowledgement")], |xml| {
                    xml.leaf("p", Some(text), &[])
                })?;
            }
            for annex in &doc.annexes {
                xml.section(annex, &[("type", "annex")])?;
            }
            if !doc.references.is_empty() {
                xml.element("div", &[("type", "references")], |xml| {
                    xml.references(&doc.references, &[])
                })?;
            }
            Ok(())
        })
    }
}

/// Resolve both byte offsets and character offsets without changing paragraph text.
/// For overlapping ranges, prefer the first start and then the longest span.
fn callout_spans(paragraph: &Paragraph) -> Vec<(usize, usize, &Callout)> {
    let text = &paragraph.text;
    let is_unicode = !paragraph.callouts.is_empty() && !text.is_ascii();
    let mut boundaries = None;
    let mut spans = Vec::new();
    for callout in &paragraph.callouts {
        let (start, end) = (callout.char_start, callout.char_end);
        if start >= end {
            continue;
        }
        let bytes = text.get(start..end);
        if is_unicode && !callout.text.is_empty() && bytes != Some(callout.text.as_str()) {
            let boundaries = boundaries.get_or_insert_with(|| {
                text.char_indices()
                    .map(|(idx, _)| idx)
                    .chain([text.len()])
                    .collect::<Vec<_>>()
            });
            if let (Some(&start), Some(&end)) = (boundaries.get(start), boundaries.get(end))
                && text[start..end] == callout.text
            {
                spans.push((start, end, callout));
                continue;
            }
        }
        if bytes.is_some() {
            spans.push((start, end, callout));
        }
    }
    spans.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
    spans
}

fn validate_xml(value: &str) -> XmlResult {
    if let Some(invalid) = value.chars().find(|ch| {
        !matches!(ch, '\u{9}' | '\u{A}' | '\u{D}' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
    }) {
        return Err(ModelError::InvalidXmlCharacter(invalid as u32));
    }
    Ok(())
}
