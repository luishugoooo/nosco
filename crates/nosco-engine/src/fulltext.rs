use std::{ops::Range, sync::LazyLock};

use nosco_crf::DecodeScratch;
use nosco_features::{FulltextInput, ReferenceCallout, extract_fulltext_input};
use nosco_models::{
    BodyElement, Callout, CalloutKind, Document, Figure, Formula, Paragraph, Section,
    Table, TableCell, TableRow,
};
use nosco_pdf::{BoundingBox, GraphicKind, LayoutDocument, LayoutToken};
use regex::Regex;

use crate::spans::{append, bbox, bboxes, clean, render, spans};
use crate::{Engine, EngineError, ModelKind};

static SECTION_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(
    r"^((?:\d+(?:\.\d+)*|[A-Z](?:\.\d+)+)[.)]?|(?:[IVXLCDM]+|[A-Z])[.)])\s+(\S.*)$"
).unwrap());
static OBJECT_LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(
    r"(?i)\b(?:(?:extended\s+data|supplementary|supp\.?)\s+)?(?:fig(?:ure)?s?\.?|tables?|tabs?\.?|schemes?|charts?|plates?)\s*([a-z]?\d+(?:\.\d+)?|[ivxlcdm]+)\b"
).unwrap());
static PLOS_OBJECT_LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(
    r"(?i)\b(s\d+)\s+(?:fig(?:ure)?\.?|table|tab\.)\b"
).unwrap());
static FIGURE_HEADER_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(
    r"(?i)^(?:(?:extended\s+data|supplementary|supp\.?)\s+)?(?:fig(?:ure)?|s\d+\s+fig|scheme|chart|plate|box)\b"
).unwrap());
static EQUATION_LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(
    r"\s*\(([A-Za-z]?\d+(?:\.\d+)?[a-z]?)\)\s*[,.;]?\s*$"
).unwrap());
static EQUATION_LABEL_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(
    r"^\s*\(([A-Za-z]?\d+(?:\.\d+)?[a-z]?)\)\s+"
).unwrap());

impl Engine {
    pub(crate) fn fulltext(
        &self, document: &mut Document, layout: &LayoutDocument, indices: &[usize],
        scratch: &mut DecodeScratch, mut next_formula: usize,
    ) -> Result<Vec<Section>, EngineError> {
        let references: Vec<_> = document.references.iter().map(|item| ReferenceCallout {
            label: item.label.as_deref(),
            first_author: item.authors.first().and_then(|p| p.last_name.as_deref()),
        }).collect();
        let input = extract_fulltext_input(layout, indices, &references);
        let labels = self.tag(ModelKind::Fulltext, &input.features, scratch)?;
        let mut sections = Vec::new();
        let mut section = Section::default();
        let mut paragraph = ParagraphBuilder::default();
        let mut spans = spans(&labels).peekable();
        let mut pending_equation_label: Option<String> = None;
        while let Some(span) = spans.next() {
            let begin = labels[span.rows.start].starts_with("I-");
            let indices = &input.token_indices[span.rows.clone()];
            if let Some(kind) = callout_kind(span.tag) {
                paragraph.marker(kind, indices);
                continue;
            }
            match span.tag {
                "<paragraph>" => {
                    if begin && paragraph.has_prose {
                        paragraph.flush(&mut section, layout, self.lexicon());
                    }
                    paragraph.indices.extend_from_slice(indices);
                    paragraph.has_prose = true;
                }
                "<section>" => {
                    paragraph.flush(&mut section, layout, self.lexicon());
                    push_section(&mut sections, &mut section);
                    let title = render(layout, indices, self.lexicon(), false).text;
                    (section.number, section.title) = section_heading(&title);
                }
                "<equation>" => {
                    paragraph.flush(&mut section, layout, self.lexicon());
                    let mut selected = indices.to_vec();
                    let mut text = render(layout, indices, self.lexicon(), false).text;
                    let mut label = pending_equation_label.take();
                    if spans.peek().is_some_and(|next| next.tag == "<equation_label>") {
                        let next = &input.token_indices[spans.next().unwrap().rows];
                        label = Some(render(layout, next, self.lexicon(), false).text);
                        selected.extend_from_slice(next);
                    } else if label.is_none()
                        && let Some(parts) = EQUATION_LABEL.captures(&text)
                    {
                        label = Some(parts[1].to_owned());
                        text.truncate(parts.get(0).unwrap().start());
                    } else if label.is_none()
                        && let Some(parts) = EQUATION_LABEL_START.captures(&text)
                    {
                        label = Some(parts[1].to_owned());
                        text = text[parts.get(0).unwrap().end()..].to_owned();
                    }
                    section.elements.push(BodyElement::Formula(Formula {
                        id: Some(format!("formula_{next_formula}")),
                        label,
                        text,
                        bboxes: bboxes(layout, &selected),
                    }));
                    next_formula += 1;
                }
                "<equation_label>" => {
                    let label = render(layout, indices, self.lexicon(), false).text;
                    if paragraph.indices.is_empty()
                        && let Some(BodyElement::Formula(formula)) = section.elements.last_mut()
                        && formula.label.is_none()
                    {
                        formula.label = Some(label);
                        formula.bboxes.extend(bboxes(layout, indices));
                    } else {
                        pending_equation_label = Some(label);
                    }
                }
                "<figure>" | "<table>" => {
                    paragraph.flush(&mut section, layout, self.lexicon());
                    let mut end = span.rows.end;
                    while spans.peek().is_some_and(|next| next.tag == span.tag
                        && labels[next.rows.start] == span.tag)
                    {
                        end = spans.next().unwrap().rows.end;
                    }
                    let indices = &input.token_indices[span.rows.start..end];
                    let kind = if span.tag == "<figure>" { ModelKind::Figure } else { ModelKind::Table };
                    let object = self.extract_object(layout, &input, indices, kind, scratch)?;
                    if kind == ModelKind::Figure {
                        let image = nearby_image(layout, indices, &document.figures);
                        let has_fig_header = object
                            .header
                            .as_deref()
                            .is_some_and(|h| FIGURE_HEADER_PREFIX.is_match(h.trim()));
                        // Discard stray non-figure CRF fragments (e.g., code blocks or affiliations)
                        // that have no label, no figure header, and no linked image.
                        if object.label.is_none() && !has_fig_header && image.is_none() {
                            continue;
                        }
                        let mut figure = Figure {
                            id: Some(format!("fig_{}", document.figures.len())),
                            header: object.header,
                            label: object.label,
                            caption: object.caption,
                            content: object.content,
                            bboxes: bboxes(layout, indices),
                        };
                        if let Some(image) = image {
                            figure.bboxes.push(bbox(image));
                        }
                        document.figures.push(figure);
                    } else {
                        let cell_indices = if !object.cells.is_empty() {
                            &object.cells
                        } else {
                            &object.fallback_cells
                        };
                        let rows = table_rows(layout, cell_indices);
                        // Discard spurious empty table fragments with no grid and no caption/label.
                        if rows.is_empty() && object.caption.is_none() && object.label.is_none() {
                            continue;
                        }
                        document.tables.push(Table {
                            id: Some(format!("tab_{}", document.tables.len())),
                            header: object.header,
                            label: object.label,
                            caption: object.caption,
                            content: object.content,
                            rows,
                            note: object.note,
                            bboxes: bboxes(layout, indices),
                        });
                    }
                }
                "<other>" => {
                    paragraph.flush(&mut section, layout, self.lexicon());
                    let value = render(layout, indices, self.lexicon(), false).text;
                    if !value.is_empty() {
                        section.elements.push(BodyElement::Note(value));
                    }
                }
                _ => {}
            }
        }
        paragraph.flush(&mut section, layout, self.lexicon());
        push_section(&mut sections, &mut section);
        Ok(nest_sections(sections))
    }

    fn extract_object(
        &self, layout: &LayoutDocument, input: &FulltextInput, indices: &[usize],
        kind: ModelKind, scratch: &mut DecodeScratch,
    ) -> Result<DisplayObject, EngineError> {
        let labels = self.tag(kind, &input.select(indices), scratch)?;
        let mut object = DisplayObject::default();
        for span in spans(&labels) {
            let indices = &indices[span.rows];
            let text = render(layout, indices, self.lexicon(), false).text;
            let field = match span.tag {
                "<figure_head>" => &mut object.header,
                "<label>" => &mut object.label,
                "<figDesc>" => &mut object.caption,
                "<note>" => &mut object.note,
                "<content>" => {
                    object.cells.extend_from_slice(indices);
                    &mut object.content
                }
                _ => {
                    if kind == ModelKind::Table {
                        object.fallback_cells.extend_from_slice(indices);
                    }
                    continue;
                }
            };
            append(field, clean(&text));
        }
        if object.label.is_none() {
            let text = render(layout, indices, self.lexicon(), false).text;
            let find_label = |s: &str| {
                let prefix: String = s.chars().take(160).collect();
                crate::identifiers::capture(&PLOS_OBJECT_LABEL, &prefix)
                    .or_else(|| crate::identifiers::capture(&OBJECT_LABEL, &prefix))
            };
            object.label = object
                .header
                .as_deref()
                .and_then(find_label)
                .or_else(|| object.caption.as_deref().and_then(find_label))
                .or_else(|| find_label(&text));
        }
        Ok(object)
    }
}

#[derive(Default)]
struct DisplayObject {
    header: Option<String>,
    label: Option<String>,
    caption: Option<String>,
    content: Option<String>,
    note: Option<String>,
    cells: Vec<usize>,
    fallback_cells: Vec<usize>,
}

fn callout_kind(tag: &str) -> Option<CalloutKind> {
    Some(match tag {
        "<citation_marker>" => CalloutKind::Bibr,
        "<figure_marker>" => CalloutKind::Figure,
        "<table_marker>" => CalloutKind::Table,
        "<equation_marker>" => CalloutKind::Formula,
        _ => return None,
    })
}

#[derive(Default)]
struct ParagraphBuilder {
    indices: Vec<usize>,
    markers: Vec<(CalloutKind, Range<usize>)>,
    has_prose: bool,
}

impl ParagraphBuilder {
    fn marker(&mut self, kind: CalloutKind, indices: &[usize]) {
        let start = self.indices.len();
        self.indices.extend_from_slice(indices);
        self.markers.push((kind, start..self.indices.len()));
    }

    fn flush(&mut self, section: &mut Section, layout: &LayoutDocument, lexicon: &nosco_lexicon::Lexicon) {
        if self.indices.is_empty() {
            return;
        }
        let rendered = render(layout, &self.indices, lexicon, false);
        let callouts = self.markers.drain(..).filter_map(|(kind, rows)| {
            let start = rendered.offsets[rows.start].start;
            let end = rendered.offsets[rows.end - 1].end;
            let text = rendered.text.get(start..end)?;
            (!text.is_empty()).then(|| Callout {
                kind,
                text: text.to_owned(),
                char_start: start,
                char_end: end,
                target_id: None,
            })
        }).collect();
        section.elements.push(BodyElement::Paragraph(Paragraph {
            text: rendered.text,
            callouts,
            bboxes: bboxes(layout, &self.indices),
        }));
        self.indices.clear();
        self.has_prose = false;
    }
}

fn section_heading(text: &str) -> (Option<String>, Option<String>) {
    let text = text.trim();
    if let Some(parts) = SECTION_NUMBER.captures(text) {
        return (Some(parts[1].trim_end_matches(['.', ')']).to_owned()), Some(parts[2].to_owned()));
    }
    (None, (!text.is_empty()).then(|| text.to_owned()))
}

fn push_section(sections: &mut Vec<Section>, section: &mut Section) {
    if section.title.is_some() || !section.elements.is_empty() {
        sections.push(std::mem::take(section));
    }
}

fn nest_sections(sections: Vec<Section>) -> Vec<Section> {
    let mut roots = Vec::new();
    let mut open: Vec<Section> = Vec::new();
    for section in sections {
        while let Some(parent) = open.last() {
            let descendant = parent.number.as_ref().zip(section.number.as_ref())
                .is_some_and(|(prefix, number)| number.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('.')));
            if descendant {
                break;
            }
            close_section(&mut open, &mut roots);
        }
        open.push(section);
    }
    while !open.is_empty() {
        close_section(&mut open, &mut roots);
    }
    roots
}

fn close_section(open: &mut Vec<Section>, roots: &mut Vec<Section>) {
    let section = open.pop().unwrap();
    if let Some(parent) = open.last_mut() {
        parent.subsections.push(section);
    } else {
        roots.push(section);
    }
}

pub(crate) fn formula_count(sections: &[Section]) -> usize {
    sections.iter().map(|s| s.elements.iter().filter(|e| matches!(e, BodyElement::Formula(_))).count()
        + formula_count(&s.subsections)).sum()
}

fn nearby_image(layout: &LayoutDocument, indices: &[usize], used: &[Figure]) -> Option<BoundingBox> {
    let first = layout.tokens.get(*indices.first()?)?.bbox;
    let bounds = indices.iter().map(|&i| layout.tokens[i].bbox).take_while(|b| b.page == first.page)
        .fold(first, |a, b| a.union(&b));
    layout.graphics.iter().filter(|image| matches!(image.kind, GraphicKind::Image)).filter_map(|image| {
        let image = image.bbox;
        if image.page != bounds.page || image.x >= bounds.x2() || bounds.x >= image.x2()
            || used.iter().any(|figure| figure.bboxes.iter().any(|b| *b == bbox(image)))
        {
            return None;
        }
        let distance = (image.y - bounds.y2()).max(bounds.y - image.y2()).max(0.0);
        (distance <= 50.0).then_some((distance, image))
    }).min_by(|(a, _), (b, _)| a.total_cmp(b)).map(|(_, bbox)| bbox)
}

#[derive(Debug)]
struct GridCell {
    text: String,
    left: f64,
    right: f64,
}

fn table_rows(layout: &LayoutDocument, indices: &[usize]) -> Vec<TableRow> {
    let mut tokens: Vec<_> = indices.iter().map(|&i| &layout.tokens[i])
        .filter(|t| !t.text.trim().is_empty() && t.bbox.x.is_finite() && t.bbox.y.is_finite())
        .collect();
    let center = |t: &LayoutToken| t.bbox.y + t.bbox.height / 2.0;
    tokens.sort_by(|a, b| a.page.cmp(&b.page).then(center(a).total_cmp(&center(b))));
    let mut visual_rows: Vec<Vec<&LayoutToken>> = Vec::new();
    for token in tokens {
        if let Some(row) = visual_rows.last_mut()
            && row[0].page == token.page
            && (center(row[0]) - center(token)).abs() <= (token.bbox.height.min(row[0].bbox.height) * 0.4).max(1.5)
        {
            row.push(token);
        } else {
            visual_rows.push(vec![token]);
        }
    }
    let mut rows: Vec<Vec<GridCell>> = Vec::new();
    for mut row in visual_rows {
        row.sort_by(|a, b| a.bbox.x.total_cmp(&b.bbox.x));
        let mut cells: Vec<GridCell> = Vec::new();
        let mut previous: Option<&LayoutToken> = None;
        for token in row {
            let text = token.text.trim();
            if let Some(cell) = cells.last_mut()
                && token.bbox.x - cell.right <= (token.font_size * 0.7).max(2.5)
            {
                if previous.is_some_and(|p| p.space_after || token.bbox.x - cell.right > token.font_size * 0.2) {
                    cell.text.push(' ');
                }
                cell.text.push_str(text);
                cell.right = cell.right.max(token.bbox.x2());
            } else {
                cells.push(GridCell { text: text.to_owned(), left: token.bbox.x, right: token.bbox.x2() });
            }
            previous = Some(token);
        }
        rows.push(cells);
    }

    // The densest row supplies the initial columns. Allow right-aligned
    // numeric cells and add columns seen only on other rows.
    let Some(anchor_row) = rows.iter().max_by_key(|row| row.len()) else { return Vec::new() };
    let mut columns: Vec<_> = anchor_row.iter().map(|c| (c.left, c.right)).collect();
    for row in &rows {
        let mut start = 0;
        for cell in row {
            if let Some(index) = aligned_column(&columns, cell, start) {
                start = index + 1;
            } else {
                let index = start + columns[start..].partition_point(|(left, _)| *left < cell.left);
                columns.insert(index, (cell.left, cell.right));
                start = index + 1;
            }
        }
    }
    rows.into_iter().enumerate().map(|(row, cells)| {
        let mut result = vec![TableCell { rows: 1, cols: 1, is_header: row == 0, ..Default::default() }; columns.len()];
        let mut start = 0;
        for cell in cells {
            if let Some(index) = aligned_column(&columns, &cell, start) {
                result[index].text = cell.text;
                start = index + 1;
            }
        }
        TableRow { cells: result }
    }).collect()
}

fn aligned_column(columns: &[(f64, f64)], cell: &GridCell, start: usize) -> Option<usize> {
    columns.iter().enumerate().skip(start).filter_map(|(i, (left, right))| {
        let distance = (left - cell.left).abs().min((right - cell.right).abs());
        (distance <= 4.0).then_some((i, distance))
    }).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spans::tests::test_token;

    #[test]
    fn parses_and_nests_numbered_sections() {
        for (text, number, title) in [("1 Introduction", "1", "Introduction"), ("2.3.1 A result", "2.3.1", "A result"),
            ("I. Methods", "I", "Methods"), ("A. Appendix", "A", "Appendix")]
        {
            assert_eq!(section_heading(text), (Some(number.into()), Some(title.into())));
        }
        let input = ["1 Introduction", "1.1 Scope", "1.1.1 Prior work", "2. Methods"].map(|heading| {
            let (number, title) = section_heading(heading);
            Section { number, title, ..Default::default() }
        });
        let tree = nest_sections(input.to_vec());
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].subsections[0].subsections[0].number.as_deref(), Some("1.1.1"));
    }

    #[test]
    fn aligns_table_columns_with_missing_cells_and_jitter() {
        let mut layout = LayoutDocument::default();
        for (text, x, y, width) in [("Name", 0.0, 10.0, 20.0), ("Value", 50.0, 10.8, 20.0),
            ("Count", 100.0, 10.2, 20.0), ("β", 0.5, 20.0, 5.0), ("4", 114.0, 20.2, 6.0)]
        {
            let mut token = test_token(text);
            token.bbox = BoundingBox::new(1, x, y, width, 8.0);
            layout.tokens.push(token);
        }
        let rows = table_rows(&layout, &[0, 4, 2, 1, 3]);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].cells.iter().all(|cell| cell.is_header));
        assert_eq!(rows[1].cells.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["β", "", "4"]);
    }
}
