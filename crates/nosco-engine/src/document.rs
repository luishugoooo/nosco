use std::collections::{HashMap, HashSet};
use std::{path::{Path, PathBuf}, sync::LazyLock};

use nosco_crf::DecodeScratch;
use nosco_features::{
    extract_header_features, extract_reference_segmenter_input, extract_segmentation_input,
};
use nosco_models::{Affiliation, BibItem, Document, Footnote, Header, Person};
use nosco_pdf::{LayoutDocument, extract_from_bytes, extract_from_file};
use rayon::prelude::*;
use regex::Regex;

use crate::identifiers::*;
use crate::spans::{append, base, bboxes, clean, compact, layout_visible, render, spans};
use crate::text::{markers, set_affiliation_keys};
use crate::{Engine, EngineError, ModelKind};

static ABSTRACT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*abstract\b[\s.:—–-]*").unwrap());
static KEYWORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*(?:key\s*words?|index terms)\b[\s.:—–-]*").unwrap());
static FIRST_SECTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:(?:1\.?|I\.)\s+)?(?:introduction|contents|background)\s*$").unwrap());
static REF_HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:[\d.]+\s+)?(?:references|bibliography|literature cited|works cited|references and notes)\s*[.:]?$").unwrap());
static BACK_HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:appendix|annex|supplementary (?:material|information))\b").unwrap());
static REF_LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\[\s*\d{1,5}\s*\]|\(\s*\d{1,5}\s*\)|\d{1,5}[.)])(?:\s|$)").unwrap());
static INLINE_LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\[\s*\d{1,5}\s*\]|\(\s*\d{1,5}\s*\)|\d{1,5}[.)])").unwrap());
static FOOTNOTE_LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\d+|[*†‡])(?:\s+|[.)]\s*)").unwrap());
static CROSSMARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bcheck\s+for\s+updates\b").unwrap());

#[derive(Clone, Copy, PartialEq, Eq)]
enum Zone {
    Header,
    Body,
    References,
    Acknowledgements,
    Annex,
    Footnote,
    Cover,
    Funding,
    Other,
}

impl Zone {
    fn from_tag(tag: &str) -> Self {
        match base(tag) {
            "<header>" => Self::Header,
            "<body>" => Self::Body,
            "<references>" => Self::References,
            "<acknowledgement>" => Self::Acknowledgements,
            "<annex>" => Self::Annex,
            "<footnote>" => Self::Footnote,
            "<cover>" => Self::Cover,
            "<funding>" => Self::Funding,
            _ => Self::Other,
        }
    }
}

struct Segmented<'a> {
    layout: &'a LayoutDocument,
    zones: Vec<Zone>,
    starts: Vec<bool>,
    furniture: Vec<bool>,
    badge: Vec<bool>,
}

impl Segmented<'_> {
    fn indices(&self, zone: Zone) -> Vec<usize> {
        self.layout.lines.iter().enumerate()
            .filter(|&(i, _)| self.zones[i] == zone && !self.badge[i])
            .flat_map(|(_, line)| line.token_range.clone())
            .filter(|&i| self.layout.tokens.get(i).is_some_and(layout_visible))
            .collect()
    }
}

impl Engine {
    pub fn process_pdf_file(&self, path: &Path) -> Result<Document, EngineError> {
        self.process_layout(&extract_from_file(path)?)
    }

    pub fn process_pdf_bytes(&self, bytes: &[u8]) -> Result<Document, EngineError> {
        self.process_layout(&extract_from_bytes(bytes)?)
    }

    pub fn process_header_file(&self, path: &Path) -> Result<Header, EngineError> {
        self.process_header_layout(&extract_from_file(path)?)
    }

    pub fn process_header_bytes(&self, bytes: &[u8]) -> Result<Header, EngineError> {
        self.process_header_layout(&extract_from_bytes(bytes)?)
    }

    pub fn process_references_file(&self, path: &Path) -> Result<Vec<BibItem>, EngineError> {
        self.process_references_layout(&extract_from_file(path)?)
    }

    pub fn process_references_bytes(&self, bytes: &[u8]) -> Result<Vec<BibItem>, EngineError> {
        self.process_references_layout(&extract_from_bytes(bytes)?)
    }

    pub fn process_batch_files(&self, paths: &[PathBuf]) -> Vec<Result<Document, EngineError>> {
        paths.par_iter().map(|path| self.process_pdf_file(path)).collect()
    }

    pub fn process_layout(&self, layout: &LayoutDocument) -> Result<Document, EngineError> {
        let mut scratch = DecodeScratch::new();
        let segments = self.segment(layout, &mut scratch)?;
        let header = self.header(&segments, &mut scratch)?;
        let references = self.references(&segments, &mut scratch)?;
        let mut document = Document { header, references, page_count: layout.pages.len(), ..Default::default() };
        document.body = self.fulltext(&mut document, layout, &segments.indices(Zone::Body), &mut scratch, 0)?;
        let formula_count = crate::fulltext::formula_count(&document.body);
        document.annexes = self.fulltext(&mut document, layout, &segments.indices(Zone::Annex), &mut scratch, formula_count)?;
        document.footnotes = self.footnotes(&segments);
        let acknowledgement = segments.indices(Zone::Acknowledgements);
        if !acknowledgement.is_empty() {
            document.acknowledgements = Some(render(layout, &acknowledgement, self.lexicon(), false).text);
        }
        crate::callouts::resolve(&mut document);
        Ok(document)
    }

    pub fn process_header_layout(&self, layout: &LayoutDocument) -> Result<Header, EngineError> {
        let mut scratch = DecodeScratch::new();
        let segments = self.segment(layout, &mut scratch)?;
        self.header(&segments, &mut scratch)
    }

    pub fn process_references_layout(&self, layout: &LayoutDocument) -> Result<Vec<BibItem>, EngineError> {
        let mut scratch = DecodeScratch::new();
        let segments = self.segment(layout, &mut scratch)?;
        self.references(&segments, &mut scratch)
    }

    fn segment<'a>(&self, layout: &'a LayoutDocument, scratch: &mut DecodeScratch) -> Result<Segmented<'a>, EngineError> {
        let input = extract_segmentation_input(layout, self.lexicon());
        let labels = self.tag(ModelKind::Segmentation, &input.features, scratch)?;
        let mut zones = vec![Zone::Other; layout.lines.len()];
        let mut starts = vec![false; layout.lines.len()];
        for (&line, label) in input.line_indices.iter().zip(labels) {
            zones[line] = Zone::from_tag(label);
            starts[line] = label.starts_with("I-");
        }

        let line_text = |i: usize| {
            let indices: Vec<_> = layout.lines[i].token_range.clone()
                .filter(|&j| layout.tokens.get(j).is_some_and(layout_visible)).collect();
            render(layout, &indices, self.lexicon(), false).text
        };

        let (furniture, badge) = detect_furniture_and_badges(layout, self.lexicon());

        if !zones.contains(&Zone::Header) {
            let cover = zones.contains(&Zone::Cover);
            let first_page = layout.lines.first().map(|line| line.page);
            for (i, zone) in zones.iter_mut().enumerate() {
                if *zone == Zone::Cover
                    || (!cover && Some(layout.lines[i].page) == first_page && i < 28
                        && matches!(*zone, Zone::Other | Zone::Body))
                {
                    if FIRST_SECTION.is_match(&line_text(i)) {
                        break;
                    }
                    *zone = Zone::Header;
                }
            }
        }

        // A recognizable bibliography heading anchors the reference tail even
        // when the segmentation model only finds the middle of the zone.
        if let Some(heading) = (0..zones.len()).rev().find(|&i| {
            matches!(zones[i], Zone::References | Zone::Body | Zone::Other)
                && REF_HEADING.is_match(line_text(i).trim())
        }) {
            for (i, zone) in zones.iter_mut().enumerate().skip(heading + 1) {
                if *zone == Zone::Annex || (*zone == Zone::Body && BACK_HEADING.is_match(&line_text(i))) {
                    break;
                }
                if matches!(*zone, Zone::Body | Zone::References) {
                    *zone = Zone::References;
                }
            }
            zones[heading] = Zone::Other;
        }
        Ok(Segmented { layout, zones, starts, furniture, badge })
    }

    fn header(&self, segments: &Segmented<'_>, scratch: &mut DecodeScratch) -> Result<Header, EngineError> {
        let layout = segments.layout;
        let indices = segments.indices(Zone::Header);
        let labels = self.tag(ModelKind::Header, &extract_header_features(layout, &indices, self.lexicon()), scratch)?;
        let mut header = Header::default();
        let mut affiliation = Vec::new();
        let mut segmented_emails = Vec::new();
        let mut abstract_indices: Vec<usize> = Vec::new();
        for span in spans(&labels) {
            let selected = &indices[span.rows];
            let text = render(layout, selected, self.lexicon(), false).text;
            let value = clean(&text);
            if span.tag != "<address>" && !affiliation.is_empty() {
                let text = render(layout, &affiliation, self.lexicon(), true).text;
                header.affiliations.extend(self.parse_affiliations_with(&text, scratch)?);
                affiliation.clear();
            }
            match span.tag {
                "<title>" => {
                    let cleaned = strip_crossmark(value);
                    append(&mut header.title, clean(&cleaned));
                }
                "<author>" => {
                    let names = render_author_tokens(layout, selected, self.lexicon());
                    if !names.trim().is_empty() {
                        header.authors.extend(self.parse_names(&names, ModelKind::NameHeader, scratch)?);
                    }
                }
                "<affiliation>" | "<address>" => affiliation.extend_from_slice(selected),
                "<abstract>" => abstract_indices.extend_from_slice(selected),
                "<keyword>" => header.keywords.extend(KEYWORDS.replace(value, "").split([',', ';', '·', '•'])
                    .map(|term| clean(term).trim_end_matches('.')).filter(|term| !term.is_empty()).map(str::to_owned)),
                "<date>" => header.dates.extend(self.parse_date_with(value, scratch)?),
                "<email>" => segmented_emails.extend(emails(value)),
                "<copyright>" => append(&mut header.copyright, value),
                "<funding>" => append(&mut header.funding_note, value),
                "<submission>" => append(&mut header.submission_note, value),
                "<meeting>" => append(&mut header.meeting, value),
                "<reference>" => append(&mut header.note, value),
                _ => {}
            }
        }
        if !abstract_indices.is_empty() {
            let trimmed = trim_abstract_column_bleed(layout, &abstract_indices);
            let raw_abs = render(layout, trimmed, self.lexicon(), false).text;
            let stripped = strip_crossmark(&ABSTRACT.replace(&raw_abs, ""));
            append(&mut header.abstract_text, clean(&stripped));
        }
        if !affiliation.is_empty() {
            let text = render(layout, &affiliation, self.lexicon(), true).text;
            header.affiliations.extend(self.parse_affiliations_with(&text, scratch)?);
        }
        if header.title.is_none() {
            header.title = recover_title_from_page1(layout, segments, self.lexicon())
                .or_else(|| layout.metadata.title.as_deref().map(compact).filter(|s| !s.is_empty()));
        }
        let page1_text = page_text_for_identifiers(layout, 1, self.lexicon());
        if header.abstract_text.is_none()
            && !header
                .title
                .as_deref()
                .is_some_and(|t| t.to_ascii_lowercase().starts_with("author correction"))
            && let Some(recovered) = recover_lead_abstract_from_page1(layout, segments, self.lexicon())
        {
            append(&mut header.abstract_text, clean(&recovered));
        }
        if header.authors.is_empty()
            && page1_text
                .to_ascii_lowercase()
                .contains("list of authors and their affiliations appears at the end")
            && let Some(names) = recover_end_of_paper_authors(layout, segments, self.lexicon())
        {
            header.authors = self.parse_names(&names, ModelKind::NameHeader, scratch)?;
        }
        if header.authors.is_empty() && let Some(names) = &layout.metadata.author {
            header.authors = self.parse_names(names, ModelKind::NameHeader, scratch)?;
        }
        dedup_authors(&mut header.authors);
        let raw = render(layout, &indices, self.lexicon(), false).text;
        header.doi = article_doi_from_links(&layout.links)
            .or_else(|| doi(&raw))
            .or_else(|| doi(&page1_text));
        header.arxiv_id = capture(&ARXIV, &raw).or_else(|| capture(&ARXIV, &page1_text));
        header.pmid = capture(&PMID, &raw).or_else(|| capture(&PMID, &page1_text));
        header.pmcid = capture(&PMCID, &raw)
            .or_else(|| capture(&PMCID, &page1_text))
            .map(|value| value.to_ascii_uppercase());
        header.issn = capture(&ISSN, &raw).or_else(|| capture(&ISSN, &page1_text));
        header.eissn = capture(&EISSN, &raw).or_else(|| capture(&EISSN, &page1_text));
        for url in urls(&raw) {
            if !header.urls.iter().any(|u| u == url) {
                header.urls.push(url.to_owned());
            }
        }
        for email in emails(&raw).into_iter().chain(emails_from_links(&layout.links)) {
            if !segmented_emails.iter().any(|e| e.eq_ignore_ascii_case(&email)) {
                segmented_emails.push(email);
            }
        }
        if header.funding_note.is_none() {
            let text = render(layout, &segments.indices(Zone::Funding), self.lexicon(), false).text;
            append(&mut header.funding_note, &text);
        }
        header.publication_date = header.dates.iter().find(|date| date.year.is_some()).cloned();
        set_affiliation_keys(&mut header.affiliations);
        link_affiliations(&mut header.authors, &header.affiliations);
        link_emails(&mut header.authors, &segmented_emails);
        link_orcid_annotations(&mut header.authors, &orcids_from_links(&layout.links));
        Ok(header)
    }

    fn references(&self, segments: &Segmented<'_>, scratch: &mut DecodeScratch) -> Result<Vec<BibItem>, EngineError> {
        let layout = segments.layout;
        let indices = segments.indices(Zone::References);
        let input = extract_reference_segmenter_input(layout, &indices, self.lexicon());
        let labels = self.tag(ModelKind::ReferenceSegmenter, &input.features, scratch)?;
        let mut chunks = Vec::new();
        let mut chunk = ReferenceChunk::default();
        let mut label_nums = Vec::new();
        for (row, &i) in input.token_indices.iter().enumerate() {
            let new_line = row == 0 || input.line_indices[row] != input.line_indices[row - 1];
            if !new_line {
                continue;
            }
            let line = input.line_indices[row];
            let indices: Vec<_> = layout.lines[line].token_range.clone().filter(|&i| layout_visible(&layout.tokens[i])).collect();
            let text = render(layout, &indices, self.lexicon(), false).text;
            if layout.tokens[i].line_idx == line
                && let Some(num) = parse_ref_label_num(&text)
            {
                label_nums.push(num);
            }
        }
        if is_numbered_bibliography(&label_nums) {
            // Line-initial monotonic numeric labels are stronger boundaries than isolated
            // token changes in an otherwise consistently numbered bibliography.
            let lines: Vec<_> = indices
                .chunk_by(|&a, &b| layout.tokens[a].line_idx == layout.tokens[b].line_idx)
                .filter_map(|line| {
                    let text = render(layout, line, self.lexicon(), false).text;
                    if REF_HEADING.is_match(text.trim()) {
                        return None;
                    }
                    let num = parse_ref_label_num(&text);
                    Some((line, num))
                })
                .collect();
            let mut last_num = 0u32;
            for (idx, (line, opt_num)) in lines.iter().enumerate() {
                if let Some(num) = *opt_num {
                    let confirmed_run = lines[idx + 1..]
                        .iter()
                        .filter_map(|(_, next)| *next)
                        .take(4)
                        .any(|next| next == num + 1);
                    let valid = num > last_num
                        && (last_num == 0 || num <= last_num + 5 || confirmed_run);
                    if valid {
                        push_chunk(&mut chunks, &mut chunk);
                        last_num = num;
                    }
                }
                chunk.text.extend_from_slice(line);
            }
        } else {
            for span in spans(&labels) {
                let indices = &input.token_indices[span.rows];
                match span.tag {
                    "<label>" => {
                        push_chunk(&mut chunks, &mut chunk);
                        chunk.label.extend_from_slice(indices);
                    }
                    "<reference>" => {
                        push_chunk(&mut chunks, &mut chunk);
                        chunk.text.extend_from_slice(indices);
                    }
                    "<other>" if !chunk.text.is_empty() => {
                        let text = render(layout, indices, self.lexicon(), false).text;
                        if !REF_HEADING.is_match(text.trim()) {
                            chunk.text.extend_from_slice(indices);
                        }
                    }
                    _ => {}
                }
            }
        }
        push_chunk(&mut chunks, &mut chunk);
        if chunks.is_empty() && !indices.is_empty() {
            for line in indices.chunk_by(|&a, &b| layout.tokens[a].block_idx == layout.tokens[b].block_idx) {
                let text = render(layout, line, self.lexicon(), false).text;
                if !REF_HEADING.is_match(text.trim()) {
                    chunks.push(ReferenceChunk { text: line.to_vec(), ..Default::default() });
                }
            }
        }
        let citations: Vec<_> = chunks.into_par_iter().map_init(DecodeScratch::new, |scratch, chunk| {
            let mut text = render(layout, &chunk.text, self.lexicon(), false).text;
            let mut label = (!chunk.label.is_empty()).then(|| render(layout, &chunk.label, self.lexicon(), false).text);
            if label.is_none()
                && parse_ref_label_num(&text).is_some()
                && let Some(parts) = INLINE_LABEL.captures(&text)
            {
                label = parts.get(1).map(|label| label.as_str().to_owned());
                text = text[parts.get(0).unwrap().end()..].trim_start().to_owned();
            }
            if text.trim().is_empty() || REF_HEADING.is_match(text.trim()) {
                return Ok(None);
            }
            let mut item = self.parse_citation_with(&text, scratch)?;
            item.label = label.filter(|label| !label.trim().is_empty());
            let mut indices = chunk.label;
            indices.extend(chunk.text);
            item.bboxes = bboxes(layout, &indices);
            Ok(Some(item))
        }).collect::<Result<_, EngineError>>()?;
        Ok(citations.into_iter().flatten().enumerate().map(|(index, mut item)| {
            item.id = Some(format!("b{index}"));
            item
        }).collect())
    }

    fn footnotes(&self, segments: &Segmented<'_>) -> Vec<Footnote> {
        let layout = segments.layout;
        let mut notes = Vec::new();
        let mut indices = Vec::new();
        let mut previous_page = None;
        let push = |notes: &mut Vec<Footnote>, indices: &mut Vec<_>, page| {
            if !indices.is_empty() {
                let text = render(layout, indices, self.lexicon(), false).text;
                let (label, body) = FOOTNOTE_LABEL.captures(&text).map_or((None, text.as_str()), |c| {
                    (c.get(1).map(|m| m.as_str().to_owned()), &text[c.get(0).unwrap().end()..])
                });
                if !body.trim().is_empty() {
                    notes.push(Footnote { id: Some(format!("foot_{}", notes.len())), label, text: body.to_owned(), page });
                }
                indices.clear();
            }
        };
        for (i, line) in layout.lines.iter().enumerate() {
            let active = segments.zones[i] == Zone::Footnote
                && !segments.furniture[i]
                && !segments.badge[i];
            if !active || previous_page != Some(line.page) || segments.starts[i] {
                push(&mut notes, &mut indices, previous_page);
            }
            if active {
                indices.extend(line.token_range.clone().filter(|&j| layout.tokens.get(j).is_some_and(layout_visible)));
                previous_page = Some(line.page);
            }
        }
        push(&mut notes, &mut indices, previous_page);
        notes
    }
}

fn strip_crossmark(text: &str) -> String {
    if !CROSSMARK.is_match(text) {
        return text.to_owned();
    }
    compact(&CROSSMARK.replace_all(text, " "))
}

fn normalize_furniture_key(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = true;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            continue;
        }
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            last_space = false;
        }
    }
    out.trim().to_owned()
}

fn detect_furniture_and_badges(
    layout: &LayoutDocument,
    lexicon: &nosco_lexicon::Lexicon,
) -> (Vec<bool>, Vec<bool>) {
    let n = layout.lines.len();
    let mut furniture = vec![false; n];
    let mut badge = vec![false; n];
    let mut key_pages: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut margin_key_pages: HashMap<String, HashSet<usize>> = HashMap::new();
    let mut line_keys: Vec<(String, bool)> = Vec::with_capacity(n);

    for (i, line) in layout.lines.iter().enumerate() {
        let indices: Vec<_> = line
            .token_range
            .clone()
            .filter(|&j| layout.tokens.get(j).is_some_and(layout_visible))
            .collect();
        let text = render(layout, &indices, lexicon, false).text;
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case("check for updates") {
            badge[i] = true;
        }
        let page_height = layout
            .pages
            .get(line.page.saturating_sub(1))
            .map_or(792.0, |p| p.height.max(1.0));
        let in_margin = line.bbox.y <= page_height * 0.14 || line.bbox.y2() >= page_height * 0.86;
        let key = normalize_furniture_key(trimmed);
        if key.len() >= 8 && key.len() <= 180 {
            key_pages.entry(key.clone()).or_default().insert(line.page);
            if in_margin {
                margin_key_pages.entry(key.clone()).or_default().insert(line.page);
            }
        }
        line_keys.push((key, in_margin));
    }

    if layout.pages.len() >= 2 {
        for (i, (key, in_margin)) in line_keys.into_iter().enumerate() {
            if key.len() < 8 || key.len() > 180 {
                continue;
            }
            let margin_hits = margin_key_pages.get(&key).map_or(0, HashSet::len);
            let total_hits = key_pages.get(&key).map_or(0, HashSet::len);
            if (in_margin && margin_hits >= 2) || total_hits >= 3 {
                furniture[i] = true;
            }
        }
    }
    (furniture, badge)
}

fn trim_abstract_column_bleed<'a>(layout: &LayoutDocument, indices: &'a [usize]) -> &'a [usize] {
    if indices.len() < 40 {
        return indices;
    }
    let first_page = layout.tokens[indices[0]].page;
    let page_width = layout
        .pages
        .get(first_page.saturating_sub(1))
        .map_or(612.0, |p| p.width.max(1.0));
    let mut line_spans: Vec<(usize, usize, f64, f64, f64, usize)> = Vec::new();
    let mut pos = 0;
    while pos < indices.len() {
        let line_idx = layout.tokens[indices[pos]].line_idx;
        let start = pos;
        let mut chars = 0usize;
        let mut max_size = 0.0f64;
        while pos < indices.len() && layout.tokens[indices[pos]].line_idx == line_idx {
            let tok = &layout.tokens[indices[pos]];
            chars += tok.text.chars().count() + 1;
            if !tok.superscript && !tok.subscript {
                max_size = max_size.max(tok.font_size);
            }
            pos += 1;
        }
        let bbox = layout.lines[line_idx].bbox;
        line_spans.push((start, pos, bbox.x, bbox.width, max_size, chars));
    }
    // Detect Nature-style wide/right-shifted summary followed directly by an unheaded 2-column body.
    let mut wide_lines = 0usize;
    let mut right_lead_lines = 0usize;
    let mut lead_size = 0.0f64;
    let mut acc_chars = 0usize;
    for (idx, &(start, _, x, width, font_size, chars)) in line_spans.iter().enumerate() {
        if x >= page_width * 0.28 && width >= page_width * 0.45 && font_size > 0.0 {
            right_lead_lines += 1;
            if lead_size == 0.0 {
                lead_size = font_size;
            }
        } else if right_lead_lines >= 4
            && acc_chars >= 300
            && x <= page_width * 0.14
            && font_size > 0.0
            && font_size <= lead_size * 0.90
        {
            return &indices[..start];
        }
        if width >= page_width * 0.52 {
            wide_lines += 1;
            acc_chars += chars;
            continue;
        }
        if wide_lines >= 3 && acc_chars >= 350 && width <= page_width * 0.48 {
            // Confirm at least 3 consecutive narrow lines (not just the last short line of a paragraph).
            let consecutive_narrow = line_spans[idx..]
                .iter()
                .take(4)
                .filter(|&&(_, _, _, w, _, _)| w <= page_width * 0.48)
                .count();
            if consecutive_narrow >= 3 {
                return &indices[..start];
            }
        }
        acc_chars += chars;
    }
    indices
}

fn recover_lead_abstract_from_page1(
    layout: &LayoutDocument,
    segments: &Segmented<'_>,
    lexicon: &nosco_lexicon::Lexicon,
) -> Option<String> {
    let page = layout.pages.first()?;
    let dominant = layout.dominant_font_size().max(7.5);
    let mut best: Option<String> = None;
    for block in &layout.blocks[page.block_range.clone()] {
        if block.bbox.x < page.width * 0.28
            || block.bbox.width < page.width * 0.48
            || block.bbox.y < page.height * 0.22
            || block.bbox.y2() > page.height * 0.85
            || block.line_range.len() < 4
        {
            continue;
        }
        let mut tokens = Vec::new();
        let mut size_sum = 0.0;
        let mut size_count = 0usize;
        for line_idx in block.line_range.clone() {
            if segments.furniture[line_idx] || segments.badge[line_idx] {
                continue;
            }
            for t in layout.lines[line_idx].token_range.clone() {
                if let Some(tok) = layout.tokens.get(t)
                    && layout_visible(tok)
                    && !tok.rotated
                {
                    tokens.push(t);
                    if !tok.superscript && !tok.subscript {
                        size_sum += tok.font_size;
                        size_count += 1;
                    }
                }
            }
        }
        if size_count < 30 {
            continue;
        }
        let avg_size = size_sum / size_count as f64;
        if avg_size < dominant * 1.10 || avg_size > dominant * 1.60 {
            continue;
        }
        let text = strip_crossmark(&render(layout, &tokens, lexicon, false).text);
        let cleaned = clean(&text);
        if cleaned.len() >= 300
            && cleaned.len() <= 3500
            && cleaned.matches(". ").count() >= 2
            && best.as_ref().is_none_or(|old| cleaned.len() > old.len())
        {
            best = Some(cleaned.to_owned());
        }
    }
    best
}

fn recover_end_of_paper_authors(
    layout: &LayoutDocument,
    segments: &Segmented<'_>,
    lexicon: &nosco_lexicon::Lexicon,
) -> Option<String> {
    let dominant = layout.dominant_font_size().max(7.5);
    let start_page = layout.pages.len().saturating_sub(2);
    let mut best_tokens: Vec<usize> = Vec::new();
    let mut best_commas = 0usize;

    for page in &layout.pages[start_page..] {
        let mut run_tokens: Vec<usize> = Vec::new();
        let mut run_commas = 0usize;
        let mut flush = |run_tokens: &mut Vec<usize>, run_commas: &mut usize| {
            if *run_commas >= 4 && *run_commas > best_commas {
                best_commas = *run_commas;
                best_tokens = std::mem::take(run_tokens);
            } else {
                run_tokens.clear();
            }
            *run_commas = 0;
        };
        for line_idx in page.line_range.clone() {
            if segments.furniture[line_idx] || segments.badge[line_idx] {
                flush(&mut run_tokens, &mut run_commas);
                continue;
            }
            let toks: Vec<_> = layout.lines[line_idx]
                .token_range
                .clone()
                .filter(|&t| {
                    layout
                        .tokens
                        .get(t)
                        .is_some_and(|tok| layout_visible(tok) && !tok.rotated)
                })
                .collect();
            if toks.is_empty() {
                continue;
            }
            let mut size_sum = 0.0;
            let mut size_count = 0usize;
            for &t in &toks {
                let tok = &layout.tokens[t];
                if !tok.superscript && !tok.subscript {
                    size_sum += tok.font_size;
                    size_count += 1;
                }
            }
            let avg_size = if size_count > 0 {
                size_sum / size_count as f64
            } else {
                0.0
            };
            let line_text = render(layout, &toks, lexicon, false).text;
            let trimmed = line_text.trim();
            let lower = trimmed.to_ascii_lowercase();
            let is_author_size = avg_size >= dominant * 1.05 && avg_size <= dominant * 1.35;
            let looks_like_names = is_author_size
                && (trimmed.contains(',') || trimmed.contains('&'))
                && !trimmed.ends_with('.')
                && !lower.starts_with("acknowledg")
                && !lower.starts_with("author contributions")
                && !lower.starts_with("competing interests")
                && !lower.starts_with("additional information")
                && !lower.starts_with("open access")
                && !lower.starts_with("publisher")
                && !lower.starts_with("references")
                && !lower.starts_with("nature communications");
            if looks_like_names {
                run_commas += trimmed.matches(',').count() + usize::from(trimmed.contains('&'));
                run_tokens.extend(toks);
            } else {
                flush(&mut run_tokens, &mut run_commas);
            }
        }
        flush(&mut run_tokens, &mut run_commas);
    }

    if best_tokens.is_empty() {
        return None;
    }
    let rendered = render_author_tokens(layout, &best_tokens, lexicon);
    (!rendered.trim().is_empty()).then_some(rendered)
}

fn recover_title_from_page1(
    layout: &LayoutDocument,
    segments: &Segmented<'_>,
    lexicon: &nosco_lexicon::Lexicon,
) -> Option<String> {
    let page = layout.pages.first()?;
    let dominant = layout.dominant_font_size().max(9.0);
    let mut candidates: Vec<(usize, f64, String)> = Vec::new();
    for i in page.line_range.clone() {
        if segments.furniture[i] || segments.badge[i] {
            continue;
        }
        let line = &layout.lines[i];
        if line.bbox.y > page.height * 0.65 {
            continue;
        }
        let toks: Vec<_> = line
            .token_range
            .clone()
            .filter(|&j| {
                layout
                    .tokens
                    .get(j)
                    .is_some_and(|t| layout_visible(t) && !t.rotated)
            })
            .collect();
        if toks.is_empty() {
            continue;
        }
        let text = compact(&render(layout, &toks, lexicon, false).text);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if ABSTRACT.is_match(trimmed) || FIRST_SECTION.is_match(trimmed) {
            break;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("arxiv:")
            || lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("doi:")
            || lower.contains('@')
        {
            continue;
        }
        let mut sizes: Vec<f64> = toks.iter().map(|&j| layout.tokens[j].font_size).collect();
        sizes.sort_unstable_by(f64::total_cmp);
        let median_size = sizes[sizes.len() / 2];
        candidates.push((i, median_size, trimmed.to_owned()));
    }
    let max_size = candidates
        .iter()
        .map(|(_, s, _)| *s)
        .fold(0.0f64, f64::max);
    if max_size < dominant * 1.15 {
        return None;
    }
    let threshold = max_size * 0.88;
    let mut parts = Vec::new();
    let mut started = false;
    let mut last_line_idx = 0usize;
    for (line_idx, size, text) in candidates {
        if size >= threshold {
            if started && line_idx > last_line_idx + 2 {
                break;
            }
            started = true;
            last_line_idx = line_idx;
            parts.push(text);
            if parts.len() >= 5 {
                break;
            }
        } else if started {
            break;
        }
    }
    let joined = clean(&parts.join(" ")).to_owned();
    (joined.len() >= 8 && joined.len() <= 320).then_some(joined)
}

fn page_text_for_identifiers(
    layout: &LayoutDocument,
    page_number: usize,
    lexicon: &nosco_lexicon::Lexicon,
) -> String {
    let Some(page) = layout.pages.get(page_number.saturating_sub(1)) else {
        return String::new();
    };
    let indices: Vec<_> = page
        .token_range
        .clone()
        .filter(|&i| layout.tokens.get(i).is_some_and(layout_visible))
        .collect();
    render(layout, &indices, lexicon, true).text
}

fn dedup_authors(authors: &mut Vec<Person>) {
    let mut result: Vec<Person> = Vec::with_capacity(authors.len());
    for candidate in std::mem::take(authors) {
        let cand_last = candidate
            .last_name
            .as_deref()
            .map(email_key)
            .unwrap_or_default();
        let cand_first = candidate
            .first_name
            .as_deref()
            .map(email_key)
            .unwrap_or_default();
        let cand_initial = cand_first.chars().next();

        let existing = if cand_last.len() >= 2 {
            result.iter_mut().find(|prev| {
                let prev_last = prev.last_name.as_deref().map(email_key).unwrap_or_default();
                if prev_last != cand_last {
                    return false;
                }
                let prev_first = prev.first_name.as_deref().map(email_key).unwrap_or_default();
                let prev_initial = prev_first.chars().next();
                match (prev_initial, cand_initial) {
                    (Some(a), Some(b)) => a == b,
                    (Some(_), None) | (None, Some(_)) | (None, None) => true,
                }
            })
        } else if !cand_first.is_empty() && cand_last.is_empty() {
            // Single-token author fragment (e.g. "Grabert" tagged as forename in correspondence).
            result.iter_mut().find(|prev| {
                prev.last_name
                    .as_deref()
                    .map(email_key)
                    .is_some_and(|last| last == cand_first)
            })
        } else {
            None
        };

        if let Some(prev) = existing {
            if prev.first_name.is_none() && candidate.first_name.is_some() {
                prev.first_name = candidate.first_name;
            }
            if prev.middle_name.is_none() && candidate.middle_name.is_some() {
                prev.middle_name = candidate.middle_name;
            }
            if prev.email.is_none() && candidate.email.is_some() {
                prev.email = candidate.email;
            }
            if prev.orcid.is_none() && candidate.orcid.is_some() {
                prev.orcid = candidate.orcid;
            }
            prev.corresp |= candidate.corresp;
            for m in candidate.affiliation_markers {
                if !prev.affiliation_markers.contains(&m) {
                    prev.affiliation_markers.push(m);
                }
            }
        } else {
            result.push(candidate);
        }
    }
    *authors = result;
}

fn link_orcid_annotations(authors: &mut [Person], orcids: &[(nosco_pdf::BoundingBox, String)]) {
    if orcids.is_empty() || authors.is_empty() {
        return;
    }
    let mut unique: Vec<String> = Vec::new();
    for (_, id) in orcids {
        if !unique.contains(id) {
            unique.push(id.clone());
        }
    }
    let missing: Vec<_> = authors.iter_mut().filter(|a| a.orcid.is_none()).collect();
    if missing.len() == unique.len() {
        for (author, orcid) in missing.into_iter().zip(unique) {
            author.orcid = Some(orcid);
        }
    }
}

fn render_author_tokens(layout: &LayoutDocument, selected: &[usize], lexicon: &nosco_lexicon::Lexicon) -> String {
    let filtered: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|&index| {
            let text = layout.tokens[index].text.as_str();
            !text.chars().all(|c| c == ']')
        })
        .collect();
    if filtered.is_empty() {
        return String::new();
    }
    let rendered = render(layout, &filtered, lexicon, false);
    if rendered
        .text
        .to_ascii_lowercase()
        .contains("list of authors and their affiliations appears at the end")
    {
        return String::new();
    }
    let mut names = String::new();
    let mut end = 0;
    for (&index, range) in filtered.iter().zip(rendered.offsets) {
        let token = &layout.tokens[index];
        if token.superscript && token.text.chars().any(char::is_alphanumeric) {
            names.push_str(&rendered.text[end..range.start]);
            names.push(' ');
            for ch in rendered.text[range.clone()].chars() {
                names.push(ch.to_digit(10).and_then(|n| "⁰¹²³⁴⁵⁶⁷⁸⁹".chars().nth(n as usize)).unwrap_or(ch));
            }
            names.push(' ');
            end = range.end;
        }
    }
    names.push_str(&rendered.text[end..]);
    names
}

#[derive(Default)]
struct ReferenceChunk {
    label: Vec<usize>,
    text: Vec<usize>,
}

fn push_chunk(chunks: &mut Vec<ReferenceChunk>, chunk: &mut ReferenceChunk) {
    if !chunk.text.is_empty() {
        chunks.push(std::mem::take(chunk));
    }
}

fn parse_ref_label_num(text: &str) -> Option<u32> {
    let cap = REF_LABEL.captures(text)?;
    let digits: String = cap[1].chars().filter(char::is_ascii_digit).collect();
    let num = digits.parse::<u32>().ok()?;
    (1..=999).contains(&num).then_some(num)
}

fn is_numbered_bibliography(nums: &[u32]) -> bool {
    if nums.len() < 2 || nums[0] > 5 {
        return false;
    }
    let increasing = nums
        .windows(2)
        .filter(|w| w[1] > w[0] && w[1] <= w[0] + 5)
        .count();
    increasing * 5 >= (nums.len() - 1) * 3
}

fn link_affiliations(authors: &mut [Person], affiliations: &[Affiliation]) {
    let affiliations: Vec<_> = affiliations.iter().filter_map(|aff| {
        Some((aff.key.as_ref()?, markers(aff.marker.as_deref().unwrap_or_default())))
    }).collect();
    for author in authors {
        let markers: Vec<_> = author.affiliation_markers.iter().flat_map(|m| markers(m)).collect();
        for (key, aff_markers) in &affiliations {
            if (affiliations.len() == 1 && markers.is_empty())
                || markers.iter().any(|m| aff_markers.contains(m))
            {
                author.affiliation_keys.push((*key).clone());
            }
        }
    }
}

fn email_key(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn link_emails(authors: &mut [Person], emails: &[String]) {
    let mut unmatched = Vec::new();
    for email in emails {
        let local = email.split_once('@').map_or("", |(local, _)| local);
        let local = email_key(local.split('+').next().unwrap_or_default());
        let mut candidates: Vec<_> = authors.iter_mut().filter_map(|author| {
            let last = email_key(author.last_name.as_deref()?);
            let first = email_key(author.first_name.as_deref().unwrap_or_default());
            if author.email.is_some() || last.is_empty() || !local.contains(&last) {
                return None;
            }
            let score = 1 + usize::from(first.chars().next().is_some_and(|initial| {
                local.starts_with(&format!("{initial}{last}")) || local.ends_with(&format!("{last}{initial}"))
            })) + usize::from(!first.is_empty() && local.contains(&first));
            Some((author, score))
        }).collect();
        candidates.sort_unstable_by_key(|(_, score)| std::cmp::Reverse(*score));
        let best_wins = match candidates.as_slice() {
            [_] => true,
            [(_, best), (_, next), ..] => best > next,
            [] => false,
        };
        if best_wins {
            candidates[0].0.email = Some(email.clone());
        } else {
            unmatched.push(email);
        }
    }
    let available: Vec<_> = authors.iter_mut().filter(|author| author.email.is_none()).collect();
    if available.len() == unmatched.len() {
        for (author, email) in available.into_iter().zip(unmatched) {
            author.email = Some(email.clone());
        }
    } else if let [email] = unmatched.as_slice() {
        let mut correspondents = available.into_iter().filter(|author| author.corresp);
        if let (Some(author), None) = (correspondents.next(), correspondents.next()) {
            author.email = Some((*email).clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_normalized_markers_and_single_unmarked_affiliation() {
        let mut authors = [Person { affiliation_markers: vec!["[¹], A, ∗".into()], ..Default::default() }, Person::default()];
        let mut affiliations = [
            Affiliation { key: Some("aff0".into()), marker: Some("1".into()), ..Default::default() },
            Affiliation { key: Some("aff1".into()), marker: Some("(a)*".into()), ..Default::default() },
        ];
        link_affiliations(&mut authors, &affiliations);
        assert_eq!(authors[0].affiliation_keys, ["aff0", "aff1"]);
        assert!(authors[1].affiliation_keys.is_empty());
        affiliations[0].marker = None;
        link_affiliations(&mut authors[1..], &affiliations[..1]);
        assert_eq!(authors[1].affiliation_keys, ["aff0"]);
    }

    #[test]
    fn numbered_bibliography_rejects_wrapped_years() {
        assert_eq!(parse_ref_label_num("2020. Evaluating models"), None);
        assert_eq!(parse_ref_label_num("1. Smith J"), Some(1));
        assert_eq!(parse_ref_label_num("[12] Doe A"), Some(12));
        assert!(is_numbered_bibliography(&[1, 2, 3, 4]));
        assert!(!is_numbered_bibliography(&[12, 15, 20]));
        assert!(!is_numbered_bibliography(&[1, 50, 2, 10]));
    }
}
