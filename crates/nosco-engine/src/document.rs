mod frontmatter;

use frontmatter::*;

use std::collections::{HashMap, HashSet};
use std::{path::{Path, PathBuf}, sync::LazyLock};

use nosco_crf::DecodeScratch;
use nosco_features::{
    extract_header_features, extract_reference_segmenter_input, extract_segmentation_input,
};
use nosco_models::{Affiliation, BibItem, Document, Footnote, Header, Person};
use nosco_pdf::{
    BoundingBox, ExtractOptions, LayoutDocument, PageInfo, extract_from_bytes,
    extract_from_bytes_with_options, extract_from_file, extract_from_file_with_options,
};
use rayon::prelude::*;
use regex::Regex;

use crate::identifiers::*;
use crate::metadata::repair_metadata_text;
use crate::spans::{append, base, bboxes, clean, compact, layout_visible, new_line, render, spans};
use crate::text::{markers, set_affiliation_keys};
use crate::{Engine, EngineError, ModelKind};

static ABSTRACT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*abstract\b[\s.:—–-]*").unwrap());
static KEYWORDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(?:key\s*words?|index terms)\b[\s.:—–-]*").unwrap());
static FIRST_SECTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:1\.?|I\.)\s+)?(?:introduction|contents|background)\s*$").unwrap()
});
static REF_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:[\d.]+\s+)?(?:references|bibliography|literature cited|works cited|references and notes)\s*[.:]?$").unwrap()
});
static BACK_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:appendix|annex|supplementary (?:material|information))\b").unwrap()
});
static CODE_REF_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\[\s*[A-Z]+[0-9]+\s*\])\s*").unwrap());
static REF_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(\[\s*\d{1,5}\s*\]|\(\s*\d{1,5}\s*\)|\d{1,5}[.)])(?:\s|$)").unwrap()
});
static INLINE_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\[\s*\d{1,5}\s*\]|\(\s*\d{1,5}\s*\)|\d{1,5}[.)])").unwrap());
static FOOTNOTE_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+|[*†‡])(?:\s+|[.)]\s*)").unwrap());
static CROSSMARK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bcheck\s+for\s+updates\b").unwrap());

const HEADER_OPTIONS: ExtractOptions = ExtractOptions { max_pages: Some(8) };
const HEADER_TOKEN_LIMIT: usize = 2500;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Zone {
    Header,
    Body,
    References,
    Acknowledgements,
    Annex,
    Footnote,
    Cover,
    Toc,
    Headnote,
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
            "<toc>" => Self::Toc,
            "<headnote>" => Self::Headnote,
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
    frontmatter: Frontmatter,
    lead_override: Vec<usize>,
}

impl Segmented<'_> {
    fn indices(&self, zone: Zone) -> Vec<usize> {
        self.layout
            .lines
            .iter()
            .enumerate()
            .filter(|&(i, _)| {
                self.zones[i] == zone
                    && !self.badge[i]
                    && (!matches!(zone, Zone::Header | Zone::Body | Zone::References)
                        || !self.furniture[i])
            })
            .flat_map(|(_, line)| line.token_range.clone())
            .filter(|&i| self.layout.tokens.get(i).is_some_and(layout_visible))
            .collect()
    }
    fn header_indices(&self) -> Vec<usize> {
        let Some(first_page) = self.frontmatter.primary() else {
            return Vec::new();
        };
        let mut indices = Vec::<usize>::new();
        let mut page = first_page;
        for (i, line) in self
            .layout
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.page >= first_page)
        {
            if line.page > first_page + 2
                || line.page > page + 1
                || self.frontmatter.excluded.contains(&line.page)
            {
                break;
            }
            page = line.page;
            if self.furniture[i] || self.badge[i] {
                continue;
            }
            let zone = if self.lead_override.contains(&i) {
                Zone::Header
            } else {
                self.zones[i]
            };
            match zone {
                Zone::Body | Zone::Toc => break,
                Zone::Header => indices.extend(
                    line.token_range
                        .clone()
                        .filter(|&j| layout_visible(&self.layout.tokens[j])),
                ),
                _ => {}
            }
            if indices.len() >= HEADER_TOKEN_LIMIT {
                indices.truncate(HEADER_TOKEN_LIMIT);
                break;
            }
        }
        indices
    }
}

impl Engine {
    pub fn process_pdf_file(&self, path: &Path) -> Result<Document, EngineError> {
        self.process_layout_with_stem(
            &extract_from_file(path)?,
            path.file_stem().and_then(|stem| stem.to_str()),
        )
    }

    pub fn process_pdf_bytes(&self, bytes: &[u8]) -> Result<Document, EngineError> {
        self.process_layout(&extract_from_bytes(bytes)?)
    }

    pub fn process_header_file(&self, path: &Path) -> Result<Header, EngineError> {
        self.process_header_layout_with_stem(
            &extract_from_file_with_options(path, HEADER_OPTIONS)?,
            path.file_stem().and_then(|stem| stem.to_str()),
        )
    }

    pub fn process_header_bytes(&self, bytes: &[u8]) -> Result<Header, EngineError> {
        self.process_header_layout(&extract_from_bytes_with_options(bytes, HEADER_OPTIONS)?)
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
        self.process_layout_with_stem(layout, None)
    }

    fn process_layout_with_stem(
        &self,
        layout: &LayoutDocument,
        file_stem: Option<&str>,
    ) -> Result<Document, EngineError> {
        let mut scratch = DecodeScratch::new();
        let segments = self.segment(layout, &mut scratch, true)?;
        let header = self.header(&segments, file_stem, &mut scratch)?;
        let references = self.references(&segments, &mut scratch)?;
        let mut document = Document { header, references, page_count: layout.pages.len(), ..Default::default() };
        document.body = self.fulltext(&mut document, layout, &segments.indices(Zone::Body), &mut scratch, 0)?;
        let formula_count = crate::fulltext::formula_count(&document.body);
        document.annexes = self.fulltext(&mut document, layout, &segments.indices(Zone::Annex), &mut scratch, formula_count)?;
        document.footnotes = self.footnotes(&segments);
        if document.references.is_empty() && !document.footnotes.is_empty() {
            self.extract_footnote_references(&mut document, &mut scratch)?;
        }
        let acknowledgement = segments.indices(Zone::Acknowledgements);
        if !acknowledgement.is_empty() {
            document.acknowledgements = Some(render(layout, &acknowledgement, self.lexicon(), false).text);
        }
        crate::callouts::resolve(&mut document);
        Ok(document)
    }

    pub fn process_header_layout(&self, layout: &LayoutDocument) -> Result<Header, EngineError> {
        self.process_header_layout_with_stem(layout, None)
    }

    fn process_header_layout_with_stem(
        &self,
        layout: &LayoutDocument,
        file_stem: Option<&str>,
    ) -> Result<Header, EngineError> {
        let mut scratch = DecodeScratch::new();
        let segments = self.segment(layout, &mut scratch, false)?;
        self.header(&segments, file_stem, &mut scratch)
    }

    pub fn process_references_layout(&self, layout: &LayoutDocument) -> Result<Vec<BibItem>, EngineError> {
        let mut scratch = DecodeScratch::new();
        let segments = self.segment(layout, &mut scratch, true)?;
        self.references(&segments, &mut scratch)
    }

    fn segment<'a>(
        &self,
        layout: &'a LayoutDocument,
        scratch: &mut DecodeScratch,
        scan_references: bool,
    ) -> Result<Segmented<'a>, EngineError> {
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

        let frontmatter = Frontmatter::detect(layout, &zones, self.lexicon());
        let mut lead_override = Vec::new();
        if let Some(page) = frontmatter.primary() {
            let visible = layout
                .lines
                .iter()
                .enumerate()
                .filter(|(i, line)| line.page == page && !furniture[*i] && !badge[*i]);
            // Segmentation sometimes tags a title page as cover or body. In that
            // case give the header model the bounded lead of the selected page.
            let has_leading_header = visible
                .clone()
                .take_while(|(i, _)| !matches!(zones[*i], Zone::Body | Zone::Toc))
                .any(|(i, _)| zones[i] == Zone::Header);
            if !has_leading_header {
                for (i, _) in visible.take(28) {
                    if zones[i] == Zone::Toc || FIRST_SECTION.is_match(&line_text(i)) {
                        break;
                    }
                    if matches!(zones[i], Zone::Other | Zone::Body | Zone::Cover) {
                        lead_override.push(i);
                    }
                }
            }
        }

        // A recognizable bibliography heading anchors the reference tail even
        // when the segmentation model only finds the middle of the zone or
        // extends an adjacent acknowledgement zone over it.
        if scan_references
            && let Some(heading) = (0..zones.len()).rev().find(|&i| {
                matches!(
                    zones[i],
                    Zone::References | Zone::Body | Zone::Acknowledgements | Zone::Other
                ) && REF_HEADING.is_match(line_text(i).trim())
                    && (layout.pages.len() < 3 || layout.lines[i].page > 1)
            })
        {
            for (i, zone) in zones.iter_mut().enumerate().take(heading) {
                if *zone == Zone::References && !furniture[i] && !badge[i] {
                    *zone = Zone::Body;
                }
            }
            for (i, zone) in zones.iter_mut().enumerate().skip(heading + 1) {
                if *zone == Zone::Annex
                    || (matches!(*zone, Zone::Body | Zone::Other)
                        && BACK_HEADING.is_match(&line_text(i)))
                {
                    break;
                }
                if !furniture[i]
                    && !badge[i]
                    && matches!(
                        *zone,
                        Zone::Body | Zone::References | Zone::Acknowledgements | Zone::Other
                    )
                {
                    *zone = Zone::References;
                }
            }
            zones[heading] = Zone::Other;
        } else if scan_references
            && !zones.iter().any(|&z| z == Zone::References)
            && zones.iter().filter(|&&z| z == Zone::Footnote).count() < 3
        {
            recover_numbered_footnotes(layout, &mut zones, &mut starts, &furniture, &badge, &line_text);
        }
        Ok(Segmented {
            layout,
            zones,
            starts,
            furniture,
            badge,
            frontmatter,
            lead_override,
        })
    }

    fn header(
        &self,
        segments: &Segmented<'_>,
        file_stem: Option<&str>,
        scratch: &mut DecodeScratch,
    ) -> Result<Header, EngineError> {
        let layout = segments.layout;
        let indices = segments.header_indices();
        let labels = self.tag(
            ModelKind::Header,
            &extract_header_features(layout, &indices, self.lexicon()),
            scratch,
        )?;
        let mut header = Header::default();
        let mut affiliation = Vec::new();
        let mut segmented_emails = Vec::new();
        let mut abstract_indices: Vec<usize> = Vec::new();
        let mut author_positions = Vec::new();
        let mut date_candidates = Vec::new();
        let mut venue_spans = Vec::new();
        let mut title_page = None;
        let raw = render(layout, &indices, self.lexicon(), true);

        for span in spans(&labels) {
            let first_row = span.rows.start;
            let selected = &indices[span.rows];
            let source_lines = || {
                selected.chunk_by(|&left, &right| {
                    !new_line(&layout.tokens[left], &layout.tokens[right])
                })
            };
            let render_line = |token: usize| {
                let range = layout.lines[layout.tokens[token].line_idx]
                    .token_range
                    .clone();
                let indices: Vec<_> = range
                    .filter(|&i| layout_visible(&layout.tokens[i]))
                    .collect();
                render(layout, &indices, self.lexicon(), false).text
            };
            let text = render(layout, selected, self.lexicon(), false).text;
            let value = clean(&text);
            if span.tag != "<address>" && !affiliation.is_empty() {
                let text = crate::spans::render_affiliation(layout, &affiliation, self.lexicon());
                header
                    .affiliations
                    .extend(self.parse_affiliations_with(&text, scratch)?);
                affiliation.clear();
            }
            match span.tag {
                "<title>" => {
                    let selected_title = title_tokens(layout, selected);
                    let title_text = (selected_title.len() != selected.len())
                        .then(|| render(layout, selected_title, self.lexicon(), false).text);
                    let stripped = strip_crossmark(title_text.as_deref().unwrap_or(value));
                    let cleaned = clean_title(&stripped);
                    let page = layout.tokens[selected[0]].page;
                    if title_page.is_none_or(|first| first == page)
                        && plausible_title(clean(cleaned))
                    {
                        title_page = Some(page);
                        append(&mut header.title, clean(cleaned));
                    }
                }
                "<author>" => {
                    let names = render_author_tokens(layout, selected, self.lexicon());
                    if !names.trim().is_empty() {
                        let people = self.parse_names(&names, ModelKind::NameHeader, scratch)?;
                        author_positions.extend(author_boxes(layout, selected, &people));
                        header.authors.extend(people);
                    }
                }
                "<editor>" => {
                    let names = EDITOR_LABEL.replace_all(value, "");
                    header.editors.extend(self.parse_names(
                        clean(&names),
                        ModelKind::NameHeader,
                        scratch,
                    )?);
                }
                "<group>" if !value.is_empty() => header.authors.push(Person {
                    organization: Some(value.to_owned()),
                    ..Default::default()
                }),
                "<affiliation>" | "<address>" => affiliation.extend_from_slice(selected),
                "<abstract>" => abstract_indices.extend_from_slice(selected),
                "<keyword>" => header.keywords.extend(
                    KEYWORDS
                        .replace(value, "")
                        .split([',', ';', '·', '•'])
                        .map(|term| clean(term).trim_end_matches('.'))
                        .filter(|term| !term.is_empty())
                        .map(str::to_owned),
                ),
                "<date>" => {
                    let mut row = first_row;
                    for line in source_lines() {
                        let start = raw.offsets[row].start;
                        row += line.len();
                        let end = raw.offsets[row - 1].end;
                        if access_notice(&render_line(line[0])) {
                            continue;
                        }
                        for (rank, value) in contextual_date_spans(&raw.text, start..end) {
                            for date in self.parse_date_with(value, scratch)? {
                                if date.year.is_some() {
                                    date_candidates.push((rank, date.clone()));
                                }
                                header.dates.push(date);
                            }
                        }
                    }
                }
                "<email>" => segmented_emails.extend(emails(value)),
                "<copyright>" => append(&mut header.copyright, value),
                "<funding>" => append(&mut header.funding_note, value),
                "<submission>" => append(&mut header.submission_note, value),
                "<meeting>" | "<reference>"
                    if plausible_venue(value)
                        && source_lines().all(|line| plausible_venue(&render_line(line[0]))) =>
                {
                    if span.tag == "<meeting>" {
                        append(&mut header.meeting, value);
                    } else {
                        venue_spans.push(value.to_owned());
                    }
                }
                _ => {}
            }
        }
        if !abstract_indices.is_empty() {
            let trimmed = trim_abstract_column_bleed(layout, &abstract_indices);
            let raw_abs = render(layout, trimmed, self.lexicon(), false).text;
            let stripped = strip_crossmark(&ABSTRACT.replace(&raw_abs, ""));
            if plausible_abstract(clean(&stripped)) {
                append(&mut header.abstract_text, clean(&stripped));
            }
        }
        if !affiliation.is_empty() {
            let text = crate::spans::render_affiliation(layout, &affiliation, self.lexicon());
            header
                .affiliations
                .extend(self.parse_affiliations_with(&text, scratch)?);
        }
        let frontmatter = &segments.frontmatter;
        let candidate_pages = || {
            frontmatter
                .candidates
                .iter()
                .filter_map(|number| layout.pages.iter().find(|page| page.number == *number))
        };
        let repair = |text| repair_metadata_text(text, self.lexicon());
        let embedded = [layout.metadata.title.as_deref(), xmp(layout, "dc:title")];
        let title_ok = |title: &String| {
            plausible_title(title) && !repeated_title(title) && !unresolved_tracked_title(title)
        };
        let embedded_titles = || {
            embedded
                .into_iter()
                .flatten()
                .map(compact)
                .map(repair)
                .map(|title| clean_title(&title).to_owned())
                .filter(|title| {
                    usable_embedded_title(title)
                        && !repeated_title(title)
                        && file_stem.is_none_or(|stem| !stem.eq_ignore_ascii_case(title))
                })
        };
        let page_title = |page| {
            recover_title_from_page(layout, segments, page, self.lexicon())
                .map(repair)
                .filter(title_ok)
        };
        let title_candidates = || candidate_pages().filter_map(page_title);
        if let Some(book) = &frontmatter.book_metadata {
            let recovered = || {
                layout
                    .pages
                    .iter()
                    .filter(|page| page.number == book.half_title_page)
                    .filter_map(page_title)
                    .chain(
                        candidate_pages()
                            .filter(|page| page.number < book.copyright_page)
                            .filter_map(page_title),
                    )
                    .find(|title| !frontmatter.is_book_series_title(title))
            };
            header.title = header
                .title
                .take()
                .map(repair)
                .filter(title_ok)
                .filter(|title| !frontmatter.is_book_series_title(title))
                .map(|title| {
                    if let Some(lead) = recovered()
                        && let Some(rest) = title
                            .strip_prefix(&lead)
                            .and_then(|rest| rest.strip_suffix(&lead))
                        && !rest.trim().is_empty()
                    {
                        format!("{lead}: {}", rest.trim())
                    } else {
                        title
                    }
                })
                .or_else(|| {
                    let cover = repair(book.title.clone());
                    if book.names.as_ref().is_some_and(|(_, editor)| *editor) {
                        // The title was isolated from an explicit contributor block or
                        // repeated on a separate title page with an editor credit.
                        return Some(cover);
                    }
                    recovered()
                        .or_else(|| embedded_titles().next())
                        .or(Some(cover))
                });
            if let Some((names, editor)) = &book.names {
                let people = self.parse_names(names, ModelKind::NameHeader, scratch)?;
                if !people.is_empty() {
                    header.authors.clear();
                    author_positions.clear();
                    if *editor {
                        header.editors = people;
                    } else if header.editors.is_empty() {
                        header.authors = people;
                    }
                }
            }
        } else {
            header.title = header
                .title
                .take()
                .map(repair)
                .map(|title| {
                    if let Some(value) = embedded.into_iter().flatten().find(|value| {
                        usable_embedded_title(value) && repairs_uppercase_spacing(&title, value)
                    }) {
                        repair(compact(value))
                    } else {
                        restore_tracked_title(title, |word| self.lexicon().in_dictionary(word))
                    }
                })
                .filter(title_ok)
                .or_else(|| title_candidates().next())
                .or_else(|| embedded_titles().next());
        }
        if frontmatter.book_metadata.is_none()
            && let Some((title, editor_line)) = frontmatter.edited_title_page()
        {
            if header.title.is_none() {
                header.title = Some(repair(title));
            }
            let editor_keys = email_key(&editor_line);
            let mut index = 0;
            while index < header.authors.len() {
                let key = email_key(&header.authors[index].full_name());
                if !key.is_empty() && editor_keys.contains(&key) {
                    header.editors.push(header.authors.remove(index));
                } else {
                    index += 1;
                }
            }
            if header.editors.is_empty() {
                header.editors = self.parse_names(&editor_line, ModelKind::NameHeader, scratch)?;
            }
        }
        if header.abstract_text.is_none()
            && !header
                .title
                .as_deref()
                .is_some_and(|title| title.to_ascii_lowercase().starts_with("author correction"))
        {
            // Unlabelled lead recovery is only safe on the selected title page.
            header.abstract_text = candidate_pages().find_map(|page| {
                recover_lead_abstract_from_page(
                    layout,
                    segments,
                    page,
                    self.lexicon(),
                    Some(page.number) == frontmatter.primary(),
                )
                .filter(|text| plausible_abstract(text))
            });
        }
        if header.authors.is_empty()
            && frontmatter
                .text(1)
                .to_ascii_lowercase()
                .contains("list of authors and their affiliations appears at the end")
            && let Some(names) = recover_end_of_paper_authors(layout, segments, self.lexicon())
        {
            header.authors = self.parse_names(&names, ModelKind::NameHeader, scratch)?;
        }
        if header.authors.is_empty() && header.editors.is_empty() {
            if let Some(creators) = layout.metadata.xmp.get("dc:creator") {
                for name in creators
                    .iter()
                    .filter(|name| plausible_metadata_author(name))
                {
                    header.authors.push(Person {
                        raw_name: Some(name.clone()),
                        ..Default::default()
                    });
                }
            }
            if header.authors.is_empty()
                && let Some(names) = &layout.metadata.author
                && plausible_metadata_author(names)
            {
                header.authors = self.parse_names(names, ModelKind::NameHeader, scratch)?;
            }
        }
        dedup_authors(&mut header.authors);
        dedup_authors(&mut header.editors);
        if frontmatter.book_metadata.is_some() {
            for contributors in [&mut header.authors, &mut header.editors] {
                if contributors.len() > 15 {
                    contributors.clear();
                }
            }
        }
        let mut links = layout
            .links
            .iter()
            .filter(|link| link.page <= 8 && !frontmatter.withdrawal_notices.contains(&link.page))
            .cloned()
            .collect::<Vec<_>>();
        links.sort_by_key(|link| (Some(link.page) != frontmatter.primary(), link.page));
        apply_identifiers(&mut header, layout, frontmatter, &links, &raw.text);
        if let Some((number, title)) = frontmatter
            .primary()
            .and_then(|page| rfc_frontmatter(frontmatter.text(page)))
        {
            let title = repair(title);
            if !title.is_empty() {
                header.title = Some(title);
            }
            header.doi = Some(format!("10.17487/RFC{number}"));
        }
        if let Some(doi) = &header.doi
            && let Some(title) = embedded_titles().find_map(|title| {
                frontmatter
                    .nist_report_pages(doi)
                    .find_map(|page| nist_report_title(&title, page))
            })
        {
            header.title = Some(title);
        }
        if frontmatter.book_metadata.is_none() {
            self.parse_header_venue(segments, &mut header, venue_spans, scratch)?;
        }
        apply_xmp_imprint(&mut header, layout);
        if frontmatter.book_metadata.is_some() {
            header.meeting = None;
            header.journal = None;
            header.volume = None;
            header.issue = None;
        } else {
            for venue in [&mut header.meeting, &mut header.journal] {
                *venue = venue
                    .take()
                    .map(repair)
                    .filter(|text| plausible_venue(text));
            }
        }
        for email in emails(&raw.text)
            .into_iter()
            .chain(emails_from_links(&links))
        {
            if !segmented_emails
                .iter()
                .any(|e| e.eq_ignore_ascii_case(&email))
            {
                segmented_emails.push(email);
            }
        }
        if header.funding_note.is_none() {
            let funding = segments
                .indices(Zone::Funding)
                .into_iter()
                .filter(|&i| Some(layout.tokens[i].page) == frontmatter.primary())
                .take(250)
                .collect::<Vec<_>>();
            let text = render(layout, &funding, self.lexicon(), false).text;
            append(&mut header.funding_note, &text);
        }
        if let Some(book) = &frontmatter.book_metadata {
            header.dates.clear();
            header.publication_date = book
                .date
                .clone()
                .or_else(|| embedded_date(layout, &["prism:publicationDate", "dc:date"]));
        } else {
            header.publication_date =
                header_publication_date(&header, layout, frontmatter, date_candidates);
        }
        set_affiliation_keys(&mut header.affiliations);
        link_affiliations(&mut header.authors, &header.affiliations);
        link_affiliations(&mut header.editors, &header.affiliations);
        link_emails(&mut header.authors, &segmented_emails);
        link_orcid_annotations(
            &mut header.authors,
            &author_positions,
            &orcids_from_links(&links),
        );
        Ok(header)
    }

    fn parse_header_venue(
        &self,
        segments: &Segmented<'_>,
        header: &mut Header,
        mut spans: Vec<String>,
        scratch: &mut DecodeScratch,
    ) -> Result<(), EngineError> {
        if self.model_if_loaded(ModelKind::Citation).is_none() {
            return Ok(());
        }
        let layout = segments.layout;
        for page in layout.pages.iter().filter(|page| {
            !segments
                .frontmatter
                .withdrawal_notices
                .contains(&page.number)
                && (page.number == 1 || Some(page.number) == segments.frontmatter.primary())
        }) {
            let mut note = Vec::new();
            for i in page.line_range.clone() {
                if segments.zones[i] == Zone::Headnote {
                    note.extend(
                        layout.lines[i]
                            .token_range
                            .clone()
                            .filter(|&i| layout_visible(&layout.tokens[i])),
                    );
                } else if !note.is_empty() {
                    spans.push(render(layout, &note, self.lexicon(), false).text);
                    note.clear();
                }
            }
            if !note.is_empty() {
                spans.push(render(layout, &note, self.lexicon(), false).text);
            }
        }
        let is_nist_report = header
            .doi
            .as_deref()
            .is_some_and(|doi| segments.frontmatter.nist_report_pages(doi).next().is_some());
        for venue in spans.iter().filter(|text| {
            (8..=600).contains(&text.chars().count()) && !boilerplate(text) && plausible_venue(text)
        }) {
            let text = if is_nist_report {
                nist_report_venue(venue)
            } else {
                std::borrow::Cow::Borrowed(venue.as_str())
            };
            apply_venue(header, self.parse_citation_with(&text, scratch)?);
        }
        Ok(())
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
        } else if let Some(indented_chunks) = hanging_indent_chunks(layout, &indices, self.lexicon()) {
            chunks = indented_chunks;
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
        let citations: Vec<_> = chunks
            .into_par_iter()
            .map_init(DecodeScratch::new, |scratch, chunk| {
                let mut text = render(layout, &chunk.text, self.lexicon(), false).text;
                let mut label = (!chunk.label.is_empty())
                    .then(|| render(layout, &chunk.label, self.lexicon(), false).text);
                if label.is_none()
                    && let Some(parts) = CODE_REF_LABEL.captures(&text).or_else(|| {
                        parse_ref_label_num(&text).and_then(|_| INLINE_LABEL.captures(&text))
                    })
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
            })
            .collect::<Result<_, EngineError>>()?;
        Ok(citations
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, mut item)| {
                item.id = Some(format!("b{index}"));
                item
            })
            .collect())
    }

    fn footnotes(&self, segments: &Segmented<'_>) -> Vec<Footnote> {
        let layout = segments.layout;
        let mut notes = Vec::new();
        let mut indices = Vec::<usize>::new();
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
            if !active {
                push(&mut notes, &mut indices, previous_page);
                continue;
            }
            let page_changed = previous_page.is_some() && previous_page != Some(line.page);
            if segments.starts[i] || (page_changed && !indices.is_empty() && {
                let line_indices: Vec<_> = line
                    .token_range
                    .clone()
                    .filter(|&j| layout.tokens.get(j).is_some_and(layout_visible))
                    .collect();
                let line_str = render(layout, &line_indices, self.lexicon(), false).text;
                FOOTNOTE_LABEL.is_match(&line_str)
            }) {
                push(&mut notes, &mut indices, previous_page);
            }
            indices.extend(line.token_range.clone().filter(|&j| layout.tokens.get(j).is_some_and(layout_visible)));
            if previous_page.is_none() || segments.starts[i] {
                previous_page = Some(line.page);
            }
        }
        push(&mut notes, &mut indices, previous_page);
        notes
    }

    fn extract_footnote_references(
        &self,
        document: &mut Document,
        scratch: &mut DecodeScratch,
    ) -> Result<(), EngineError> {
        static IBID: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?i)^(?:ibid|id)\b\.?").unwrap());
        static SIGNAL: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"(?i)^(?:(?:and\s+)?(?:see\s+also|see|cf\.|e\.g\.,?|for\s+[^,]{1,80},?\s+see|cited\s+in|quoted\s+in|on\s+[^,]{1,60},?\s+see)\s+|and\s+)",
            )
            .unwrap()
        });
        static YEAR: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\b(?:18|19|20)\d{2}\b").unwrap());
        static DOI_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"(?i)(?:doi\.org/|10\.\d{4,9}/)").unwrap());

        let mut parsed_refs: Vec<BibItem> = Vec::new();
        let mut remaining_footnotes: Vec<Footnote> = Vec::new();
        let mut previous_primary: Option<BibItem> = None;

        for note in std::mem::take(&mut document.footnotes) {
            let Some(label) = note.label.as_deref().filter(|l| l.chars().all(|c| c.is_ascii_digit())) else {
                remaining_footnotes.push(note);
                continue;
            };
            let trimmed = note.text.trim();
            if IBID.is_match(trimmed)
                && let Some(prev) = &previous_primary
            {
                let mut item = prev.clone();
                item.id = Some(format!("b{}", parsed_refs.len()));
                item.label = Some(label.to_owned());
                item.raw_citation = trimmed.to_owned();
                parsed_refs.push(item);
                continue;
            }

            let clauses = split_footnote_clauses(trimmed);
            let mut note_refs = Vec::new();
            for clause in clauses {
                let stripped = SIGNAL.replace(clause, "").trim().to_owned();
                if stripped.len() < 12 {
                    continue;
                }
                let has_quote = stripped.contains('“') || stripped.contains('"');
                let has_year = YEAR.is_match(&stripped);
                let has_doi = DOI_RE.is_match(&stripped);
                let short_prior = find_prior_ref_by_title(&stripped, &parsed_refs);
                if !has_quote && !has_year && !has_doi && short_prior.is_none() {
                    continue;
                }
                if let Some(prior) = short_prior.filter(|_| !has_year && !has_doi) {
                    let mut item = prior.clone();
                    item.id = Some(format!("b{}", parsed_refs.len() + note_refs.len()));
                    item.label = Some(label.to_owned());
                    item.raw_citation = clause.to_owned();
                    note_refs.push(item);
                    continue;
                }
                let mut item = self.parse_citation_with(&stripped, scratch)?;
                let has_structured = item.title.is_some()
                    || !item.authors.is_empty()
                    || item.doi.is_some()
                    || item.journal.is_some();
                if !has_structured {
                    continue;
                }
                if item.date.is_none()
                    && item.journal.is_none()
                    && let Some(title) = item.title.as_deref()
                    && let Some(prior) = find_prior_ref_by_title(title, &parsed_refs)
                {
                    if item.date.is_none() {
                        item.date = prior.date.clone();
                    }
                    if item.journal.is_none() {
                        item.journal = prior.journal.clone();
                    }
                    if item.doi.is_none() {
                        item.doi = prior.doi.clone();
                    }
                    if item.authors.is_empty() {
                        item.authors = prior.authors.clone();
                    }
                }
                item.id = Some(format!("b{}", parsed_refs.len() + note_refs.len()));
                item.label = Some(label.to_owned());
                item.raw_citation = clause.to_owned();
                note_refs.push(item);
            }

            if note_refs.is_empty() {
                remaining_footnotes.push(note);
            } else {
                previous_primary = note_refs.first().cloned();
                parsed_refs.extend(note_refs);
            }
        }

        if !parsed_refs.is_empty() {
            document.references = parsed_refs;
            document.footnotes = remaining_footnotes;
        } else {
            document.footnotes = remaining_footnotes;
        }
        Ok(())
    }
}

fn hanging_indent_chunks(
    layout: &LayoutDocument,
    indices: &[usize],
    lexicon: &nosco_lexicon::Lexicon,
) -> Option<Vec<ReferenceChunk>> {
    // Group reference tokens into visual lines, preserving page & x-coordinate.
    let mut page_lines: Vec<(usize, Vec<(f64, Vec<usize>)>)> = Vec::new();
    for line_tokens in indices.chunk_by(|&a, &b| layout.tokens[a].line_idx == layout.tokens[b].line_idx) {
        let line_idx = layout.tokens[line_tokens[0]].line_idx;
        let line = &layout.lines[line_idx];
        let text = render(layout, line_tokens, lexicon, false).text;
        if text.trim().is_empty() || REF_HEADING.is_match(text.trim()) {
            continue;
        }
        let x = line.bbox.x;
        if page_lines.last().is_none_or(|(p, _)| *p != line.page) {
            page_lines.push((line.page, Vec::new()));
        }
        page_lines.last_mut().unwrap().1.push((x, line_tokens.to_vec()));
    }

    let total_lines: usize = page_lines.iter().map(|(_, lines)| lines.len()).sum();
    if total_lines < 8 {
        return None;
    }

    // Check per-page left-margin clustering (handles alternating odd/even journal margins).
    let mut indented_total = 0usize;
    let mut margin_total = 0usize;
    let mut page_bases: Vec<f64> = Vec::with_capacity(page_lines.len());
    for (_, lines) in &page_lines {
        let mut xs: Vec<f64> = lines.iter().map(|(x, _)| *x).collect();
        xs.sort_by(|a, b| a.total_cmp(b));
        let base_x = xs[xs.len() / 10];
        // Require a single-column-like layout on each page (indented lines cluster within ~40pt of base_x).
        let max_indent = xs.iter().copied().fold(0.0f64, |acc, x| acc.max(x - base_x));
        if max_indent > 60.0 {
            return None;
        }
        for &x in &xs {
            if x <= base_x + 4.5 {
                margin_total += 1;
            } else if x >= base_x + 6.0 {
                indented_total += 1;
            }
        }
        page_bases.push(base_x);
    }

    if margin_total < 4 || indented_total < (total_lines * 15 / 100).max(2) {
        return None;
    }

    let mut chunks = Vec::new();
    let mut current = ReferenceChunk::default();
    for ((_, lines), base_x) in page_lines.into_iter().zip(page_bases) {
        for (x, tokens) in lines {
            if x <= base_x + 4.5 {
                push_chunk(&mut chunks, &mut current);
            }
            current.text.extend(tokens);
        }
    }
    push_chunk(&mut chunks, &mut current);
    (!chunks.is_empty()).then_some(chunks)
}

fn line_font_size(layout: &LayoutDocument, line_idx: usize) -> f64 {
    let mut sizes: Vec<f64> = layout.lines[line_idx]
        .token_range
        .clone()
        .filter_map(|j| layout.tokens.get(j))
        .filter(|t| layout_visible(t) && t.font_size > 0.0)
        .map(|t| t.font_size)
        .collect();
    if sizes.is_empty() {
        return 0.0;
    }
    sizes.sort_by(|a, b| a.total_cmp(b));
    sizes[sizes.len() / 2]
}

fn recover_numbered_footnotes(
    layout: &LayoutDocument,
    zones: &mut [Zone],
    starts: &mut [bool],
    furniture: &[bool],
    badge: &[bool],
    line_text: &impl Fn(usize) -> String,
) {
    static NUMBERED_NOTE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*(\d{1,3})\.\s+\S").unwrap());
    static FOOTER_NOISE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(?:this content downloaded from|all use subject to|downloaded from|https?://about\.jstor\.org|©\s*\d{4}\b|\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\s+on\b)")
            .unwrap()
    });

    // Compute dominant body font size from upper-half lines across the document.
    let mut size_counts: HashMap<u32, usize> = HashMap::new();
    for (i, line) in layout.lines.iter().enumerate() {
        if furniture[i] || badge[i] || zones[i] != Zone::Body {
            continue;
        }
        let Some(page) = layout.pages.get(line.page.saturating_sub(1)) else {
            continue;
        };
        if line.bbox.y > page.height * 0.55 {
            continue;
        }
        let text = line_text(i);
        if text.len() < 30 {
            continue;
        }
        let sz = (line_font_size(layout, i) * 2.0).round() as u32;
        if sz > 0 {
            *size_counts.entry(sz).or_default() += 1;
        }
    }
    let body_size = size_counts
        .into_iter()
        .max_by_key(|&(_, count)| count)
        .map_or(10.0, |(bucket, _)| f64::from(bucket) / 2.0);
    let max_fn_size = body_size * 0.92;
    let min_fn_size = 5.5;

    let mut last_num = 0u32;
    let mut recovered_notes = 0usize;
    let mut candidates: Vec<(usize, bool)> = Vec::new();

    for page in &layout.pages {
        let mut small_lines: Vec<(usize, String)> = Vec::new();
        for i in page.line_range.clone() {
            if furniture[i] || badge[i] {
                continue;
            }
            if !matches!(zones[i], Zone::Body | Zone::Other | Zone::Footnote) {
                continue;
            }
            let line = &layout.lines[i];
            if line.bbox.y < page.height * 0.28 || line.bbox.y > page.height - 50.0 {
                continue;
            }
            let sz = line_font_size(layout, i);
            if sz > max_fn_size || sz < min_fn_size {
                continue;
            }
            let text = line_text(i);
            let trimmed = text.trim();
            if trimmed.is_empty() || FOOTER_NOISE.is_match(trimmed) {
                continue;
            }
            if sz <= body_size * 0.75 && trimmed.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            small_lines.push((i, trimmed.to_owned()));
        }
        if small_lines.is_empty() {
            continue;
        }

        let first_note_idx = small_lines.iter().position(|(_, text)| {
            NUMBERED_NOTE
                .captures(text)
                .and_then(|c| c.get(1)?.as_str().parse::<u32>().ok())
                .is_some_and(|num| num > last_num && num <= last_num + 15)
        });

        // Lines before the first numbered footnote on this page may continue the previous page's note.
        if recovered_notes > 0 {
            let cont_end = first_note_idx.unwrap_or(small_lines.len());
            if let Some((first_line, first_text)) = small_lines.first()
                && cont_end > 0
                && layout.lines[*first_line].bbox.y > page.height * 0.42
                && first_text.len() >= 15
            {
                for &(line_idx, _) in &small_lines[..cont_end] {
                    candidates.push((line_idx, false));
                }
            }
        }

        let Some(start_idx) = first_note_idx else {
            continue;
        };
        for (line_idx, text) in &small_lines[start_idx..] {
            if let Some(num) = NUMBERED_NOTE
                .captures(text)
                .and_then(|c| c.get(1)?.as_str().parse::<u32>().ok())
                .filter(|&num| num > last_num && num <= last_num + 15)
            {
                last_num = num;
                recovered_notes += 1;
                candidates.push((*line_idx, true));
            } else if recovered_notes > 0 {
                candidates.push((*line_idx, false));
            }
        }
    }

    if recovered_notes >= 3 {
        for (line_idx, is_start) in candidates {
            zones[line_idx] = Zone::Footnote;
            starts[line_idx] = is_start;
        }
    }
}

fn split_footnote_clauses(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut in_quote = false;
    let mut start = 0usize;
    for (idx, ch) in text.char_indices() {
        match ch {
            '“' | '"' => in_quote = !in_quote,
            '”' => in_quote = false,
            '(' | '[' if !in_quote => depth += 1,
            ')' | ']' if !in_quote => depth = depth.saturating_sub(1),
            ';' if !in_quote && depth == 0 => {
                let chunk = text[start..idx].trim();
                if !chunk.is_empty() {
                    parts.push(chunk);
                }
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

fn find_prior_ref_by_title<'a>(text: &str, prior: &'a [BibItem]) -> Option<&'a BibItem> {
    let lower = text.to_lowercase();
    prior.iter().find(|item| {
        let Some(title) = item.title.as_deref() else {
            return false;
        };
        let title_lower = title.to_lowercase();
        let prefix: String = title_lower.chars().take(24).collect();
        prefix.len() >= 10 && lower.contains(&prefix)
    })
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

fn recover_lead_abstract_from_page(
    layout: &LayoutDocument,
    segments: &Segmented<'_>,
    page: &PageInfo,
    lexicon: &nosco_lexicon::Lexicon,
    allow_unlabelled: bool,
) -> Option<String> {
    if let Some(text) = explicit_abstract(layout, segments, page, lexicon) {
        return Some(text);
    }
    if !allow_unlabelled {
        return None;
    }
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

fn title_tokens<'a>(layout: &LayoutDocument, mut indices: &'a [usize]) -> &'a [usize] {
    while let Some((&last, rest)) = indices.split_last() {
        let token = &layout.tokens[last];
        if !matches!(token.text.as_str(), "†" | "‡" | "*")
            && !(token.superscript && token.text == "q")
        {
            break;
        }
        indices = rest;
    }
    indices
}

fn small_caps_title(
    layout: &LayoutDocument,
    indices: &[usize],
    lexicon: &nosco_lexicon::Lexicon,
) -> Option<(String, f64)> {
    let mut words = String::new();
    let mut largest: f64 = 0.0;
    let mut joined = 0;
    let mut tokens = indices.iter().map(|&idx| &layout.tokens[idx]).peekable();
    while let Some(token) = tokens.next() {
        if !token.text.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return None;
        }
        if !words.is_empty() {
            words.push(' ');
        }
        let start = words.len();
        words.push_str(&token.text);
        if let Some(next) = tokens.peek()
            && token.text.len() == 1
            && next.font_size < token.font_size * 0.85
            && next.text.bytes().all(|byte| byte.is_ascii_uppercase())
        {
            words.push_str(&next.text);
            if lexicon.in_dictionary(&words[start..]) {
                largest = largest.max(token.font_size);
                joined += 1;
                tokens.next();
            } else {
                words.truncate(start + token.text.len());
            }
        }
    }
    (joined >= 2).then_some((words, largest))
}

fn recover_title_from_page(
    layout: &LayoutDocument,
    segments: &Segmented<'_>,
    page: &PageInfo,
    lexicon: &nosco_lexicon::Lexicon,
) -> Option<String> {
    let dominant = layout.dominant_font_size().max(9.0);
    let mut candidates: Vec<(usize, f64, String)> = Vec::new();
    for i in page.line_range.clone() {
        if segments.badge[i] {
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
        let toks = title_tokens(layout, &toks);
        if toks.is_empty() {
            continue;
        }
        let small_caps = small_caps_title(layout, toks, lexicon);
        // Some journals repeat the title in their running headers. A prominent
        // small-caps title on the title page can share the same furniture key.
        if segments.furniture[i]
            && !small_caps
                .as_ref()
                .is_some_and(|(_, size)| *size >= dominant * 1.4)
        {
            continue;
        }
        let text = small_caps.as_ref().map_or_else(
            || compact(&render(layout, toks, lexicon, false).text),
            |(text, _)| text.clone(),
        );
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if ABSTRACT.is_match(trimmed)
            || FIRST_SECTION.is_match(trimmed)
            || NUMBERED_SECTION.is_match(trimmed)
        {
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
        let size = small_caps.map_or(sizes[sizes.len() / 2], |(_, size)| size);
        let title = clean_title(trimmed);
        if plausible_title(title) {
            candidates.push((i, size, title.to_owned()));
        }
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
    let Some(page) = layout.pages.iter().find(|page| page.number == page_number) else {
        return String::new();
    };
    let indices: Vec<_> = page
        .token_range
        .clone()
        .filter(|&i| layout.tokens.get(i).is_some_and(layout_visible))
        .collect();
    render(layout, &indices, lexicon, true).text
}

fn explicit_abstract(
    layout: &LayoutDocument,
    segments: &Segmented<'_>,
    page: &PageInfo,
    lexicon: &nosco_lexicon::Lexicon,
) -> Option<String> {
    let mut indices = Vec::<usize>::new();
    let mut started = false;
    for i in page.line_range.clone() {
        if segments.furniture[i] || segments.badge[i] {
            continue;
        }
        let line = &layout.lines[i];
        let tokens: Vec<_> = line
            .token_range
            .clone()
            .filter(|&i| layout_visible(&layout.tokens[i]))
            .collect();
        let text = render(layout, &tokens, lexicon, false).text;
        if !started {
            started = ABSTRACT.is_match(&text);
        } else if KEYWORDS.is_match(&text)
            || FIRST_SECTION.is_match(&text)
            || segments.zones[i] == Zone::Toc
            || (line.block_idx != layout.tokens[indices[0]].block_idx
                && segments.zones[i] == Zone::Body)
        {
            break;
        }
        if started {
            indices.extend(tokens);
            if indices.len() > 650 {
                return None;
            }
        }
    }
    let text = render(layout, &indices, lexicon, false).text;
    let result = ABSTRACT.replace(&text, "");
    plausible_abstract(clean(&result)).then(|| clean(&result).to_owned())
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

fn link_orcid_annotations(
    authors: &mut [Person],
    positions: &[(String, BoundingBox)],
    orcids: &[(BoundingBox, String)],
) {
    for (link, id) in orcids {
        if authors
            .iter()
            .any(|person| person.orcid.as_ref() == Some(id))
        {
            continue;
        }
        let nearest = positions
            .iter()
            .filter_map(|(name, bbox)| {
                if bbox.page != link.page {
                    return None;
                }
                let dx = (bbox.x - link.x2()).max(link.x - bbox.x2()).max(0.0);
                let dy = ((bbox.y + bbox.y2()) - (link.y + link.y2())).abs() / 2.0;
                (dx <= 48.0 && dy <= bbox.height.max(link.height) * 0.8)
                    .then_some((name, dx + 3.0 * dy))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((name, _)) = nearest
            && let Some(author) = authors
                .iter_mut()
                .find(|person| person.full_name() == *name && person.orcid.is_none())
        {
            author.orcid = Some(id.clone());
        }
    }
}

fn author_boxes(
    layout: &LayoutDocument,
    indices: &[usize],
    people: &[Person],
) -> Vec<(String, BoundingBox)> {
    let mut tokens = indices.iter().map(|&i| &layout.tokens[i]);
    let mut positions = Vec::new();
    for person in people {
        let Some(last) = person
            .last_name
            .as_deref()
            .and_then(|name| name.split_whitespace().last())
        else {
            continue;
        };
        let mut search = tokens.clone();
        if let Some(token) = search.find(|token| {
            token
                .text
                .trim_matches(|ch: char| !ch.is_alphabetic())
                .eq_ignore_ascii_case(last)
        }) {
            positions.push((person.full_name(), token.bbox));
            tokens = search;
        }
    }
    positions
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
    use crate::text::year;
    use nosco_models::Date;

    fn page_layout(contents: &[&[&str]]) -> LayoutDocument {
        let mut layout = LayoutDocument::default();
        for (index, lines) in contents.iter().enumerate() {
            let page = index + 1;
            let line_start = layout.lines.len();
            let start = layout.tokens.len();
            let bbox = BoundingBox::new(page, 0., 0., 400., 600.);
            for text in *lines {
                let index = layout.tokens.len();
                let mut token = crate::spans::tests::test_token(text);
                token.page = page;
                token.line_idx = layout.lines.len();
                layout.tokens.push(token);
                layout.lines.push(nosco_pdf::LayoutLine {
                    page,
                    bbox,
                    token_range: index..index + 1,
                    block_idx: 0,
                });
            }
            layout.pages.push(PageInfo {
                number: page,
                width: 400.,
                height: 600.,
                main_area: bbox,
                block_range: 0..0,
                line_range: line_start..layout.lines.len(),
                token_range: start..layout.tokens.len(),
            });
        }
        layout
    }

    #[test]
    fn excludes_repository_cover_but_keeps_its_identifiers() {
        let layout = page_layout(&[
            &[
                "Author(s): Example",
                "Source: Example Journal",
                "Stable URL: https://jstor.org/stable/1234",
            ],
            &[
                "The actual article title",
                "Example Author",
                "Abstract: The article begins here.",
            ],
            &["HAL Id: hal-00000001", "Submitted on 2 January 2020"],
            &[
                "All rights reserved",
                "Copyright 2001",
                "ISBN 978-0-387-32458-6",
            ],
        ]);
        let frontmatter = Frontmatter::detect(&layout, &[Zone::Header; 11], &Default::default());
        assert_eq!(frontmatter.primary(), Some(2));
        assert!(repository_cover(frontmatter.text(3)));
        assert!(frontmatter.excluded.contains(&4));
        let mut header = Header::default();
        apply_identifiers(&mut header, &layout, &frontmatter, &[], "");
        assert_eq!(header.urls, ["https://jstor.org/stable/1234"]);
        assert_eq!(header.isbn.as_deref(), Some("9780387324586"));
        assert_eq!(
            copyright_date(frontmatter.ordered_text()).unwrap().year,
            Some(2001)
        );
    }

    #[test]
    fn withdrawal_cover_does_not_supply_replacement_metadata() {
        for wording in [
            "The attached publication has been withdrawn.",
            "This publication has been withdrawn.",
        ] {
            let layout = page_layout(&[
                &[
                    "Withdrawal notice",
                    wording,
                    "It may have been superseded by another publication.",
                    "DOI: 10.1234/replacement123",
                    "Copyright 2025",
                ],
                &[
                    "The original report",
                    "DOI: 10.1234/original123",
                    "Copyright 2001",
                ],
            ]);
            let frontmatter = Frontmatter::detect(&layout, &[Zone::Header; 8], &Default::default());
            assert_eq!(frontmatter.primary(), Some(2));
            assert!(frontmatter.withdrawal_notices.contains(&1));
            let mut header = Header::default();
            apply_identifiers(&mut header, &layout, &frontmatter, &[], "");
            assert_eq!(header.doi.as_deref(), Some("10.1234/original123"));
            assert_eq!(
                copyright_date(frontmatter.ordered_text()).unwrap().year,
                Some(2001)
            );
        }
    }

    #[test]
    fn recovers_rfc_titles_from_mastheads() {
        assert_eq!(
            rfc_frontmatter(
                "Internet Engineering Task Force\nRFC 9114\nHTTP/3\nAbstract\nDescription\nStatus of this Memo"
            ),
            Some(("9114", "HTTP/3".into()))
        );
        assert_eq!(
            rfc_frontmatter(
                "Network Working Group\nRequest for Comments: 1951   P. Deutsch\nMay 1996\nDEFLATE specification\nStatus of This Memo"
            ),
            Some(("1951", "DEFLATE specification".into()))
        );
        assert!(
            rfc_frontmatter(
                "A Survey\nDOI: 10.1234/survey123\nRFC 9114\nHTTP/3\nStatus of This Memo"
            )
            .is_none()
        );
    }

    #[test]
    fn corroborates_embedded_report_titles_on_the_page() {
        let page = "NIST Special Publication 800-145\nThe NIST Definition of Cloud\nComputing";
        assert_eq!(
            nist_report_title(
                "NIST SP 800-145, The NIST Definition of Cloud Computing",
                page
            )
            .as_deref(),
            Some("The NIST Definition of Cloud Computing")
        );
        assert!(nist_report_title("A different title", page).is_none());
    }

    #[test]
    fn skips_book_half_title_and_bounds_header_lines() {
        let layout = page_layout(&[
            &["A handbook"],
            &[
                "A handbook",
                "Edited by Some Person",
                "Running head",
                "Body starts",
                "Stray header",
            ],
            &["Contents", "Unrelated chapter title"],
            &["Copyright 2001", "ISBN 978-0-387-32458-6"],
        ]);
        let zones = vec![
            Zone::Header,
            Zone::Header,
            Zone::Header,
            Zone::Header,
            Zone::Body,
            Zone::Header,
            Zone::Toc,
            Zone::Toc,
            Zone::Other,
            Zone::Other,
        ];
        let frontmatter = Frontmatter::detect(&layout, &zones, &Default::default());
        assert_eq!(frontmatter.primary(), Some(2));
        let mut furniture = vec![false; zones.len()];
        furniture[3] = true;
        let segments = Segmented {
            layout: &layout,
            starts: vec![false; zones.len()],
            badge: vec![false; zones.len()],
            zones,
            furniture,
            frontmatter,
            lead_override: Vec::new(),
        };
        assert_eq!(segments.header_indices(), [1, 2]);
        assert!(!segments.indices(Zone::Header).contains(&3));
    }

    #[test]
    fn bounds_header_by_token_limit_page_window_and_toc() {
        let lexicon = Default::default();
        let long_page = vec!["word"; HEADER_TOKEN_LIMIT + 4];
        let layout = page_layout(&[&long_page, &["lead"], &["lead"], &["late"]]);
        let zones = vec![Zone::Header; layout.lines.len()];
        let mut segments = Segmented {
            layout: &layout,
            starts: vec![false; zones.len()],
            furniture: vec![false; zones.len()],
            badge: vec![false; zones.len()],
            frontmatter: Frontmatter::detect(&layout, &zones, &lexicon),
            zones,
            lead_override: Vec::new(),
        };
        assert_eq!(segments.header_indices().len(), HEADER_TOKEN_LIMIT);
        segments.furniture[..long_page.len() - 1].fill(true);
        assert_eq!(segments.header_indices().len(), 3);
        segments.zones[long_page.len()] = Zone::Toc;
        assert_eq!(segments.header_indices().len(), 1);
    }

    #[test]
    fn rejects_boilerplate_titles_and_abstracts() {
        for title in [
            "Microsoft Word - title.docx",
            "PII: 244329",
            "Untitled",
            "Layout 1",
            "chapter.pdf",
            "Chapter one. Another heading. A third title. Some more headings. Another chapter",
        ] {
            assert!(!plausible_title(title), "{title}");
        }
        assert!(plausible_title("The science of social movements"));
        for text in [
            "All rights reserved. This material may not be translated or copied.",
            "Downloaded from an archive. Terms of use apply.",
            "), except for brief excerpts when referenced in a review",
        ] {
            assert!(!plausible_abstract(text), "{text}");
        }
        assert!(plausible_abstract(
            "We analyse the history of social movement theories."
        ));
    }

    #[test]
    fn rejects_section_headings_and_series_banners_as_titles() {
        for text in [
            "2. Background",
            "2. Method",
            "3. Specializing Multi-Step Reasoning",
            "3.1 Related Work",
            "4.2.1. Conclusions",
            "DISCUSSION PAPER SERIES",
            "WORKING PAPER SERIES",
            "NBER WORKING PAPER SERIES",
            "Working Paper Number 42",
            "IAB-KURZBERICHT",
            "This page intentionally left blank",
            "Online Appendix",
            "Supplementary Material",
        ] {
            assert!(!plausible_title(text), "{text}");
        }
        for prefix in [
            "NBER WORKING PAPER SERIES",
            "IZA DISCUSSION PAPER SERIES",
            "DISCUSSION PAPER SERIES",
        ] {
            assert_eq!(
                clean_title(&format!("{prefix} An Actual Paper")),
                "An Actual Paper"
            );
        }
        let tracked = "G LO B A L I N E Q UA L I T Y";
        assert_eq!(
            restore_tracked_title(tracked.into(), |word| matches!(
                word,
                "GLOBAL" | "INEQUALITY" | "EQUALITY"
            )),
            "GLOBAL INEQUALITY"
        );
        assert_eq!(restore_tracked_title(tracked.into(), |_| false), tracked);
        assert!(repairs_uppercase_spacing(
            "GLOBAL IN EQUALITY",
            "Global Inequality"
        ));
        assert!(!repairs_uppercase_spacing(
            "GLOBAL ECONOMY",
            "Global Inequality"
        ));
        assert_eq!(
            restore_tracked_title("THE US IN AN EU AI LAW".into(), |_| true),
            "THE US IN AN EU AI LAW"
        );
    }

    #[test]
    fn access_notices_do_not_supply_publication_dates() {
        for marker in [
            "Accessed:",
            "Downloaded",
            "Téléchargé",
            "Retrieved",
            "Printed",
            "Stable URL",
            "All content following this page",
            "Scholars Portal",
        ] {
            let before = format!("{marker} 2016");
            assert!(
                contextual_date_spans(&before, before.len() - 4..before.len()).is_empty(),
                "{before}"
            );
            let after = format!("2016 {marker}");
            assert!(contextual_date_spans(&after, 0..4).is_empty(), "{after}");
        }

        let mut layout = LayoutDocument::default();
        layout.metadata.creation_date = Some("D:20160901000000".into());
        layout
            .metadata
            .xmp
            .insert("xmp:CreateDate".into(), vec!["2016-09-01".into()]);
        for notice in [
            "JSTOR is a not-for-profit",
            "Stable URL: https://www.jstor.org/stable/123",
            "Downloaded from Cambridge Core",
            "Downloaded from ProQuest",
            "Downloaded from EBSCO",
            "All content following this page was uploaded to ResearchGate",
            "Scholars Portal Books",
        ] {
            let cover = page_layout(&[&[notice]]);
            let frontmatter = Frontmatter::detect(&cover, &[Zone::Header], &Default::default());
            assert!(
                header_publication_date(&Header::default(), &layout, &frontmatter, vec![])
                    .is_none(),
                "{notice}"
            );
            let book = page_layout(&[&[notice], &["Copyright © 1984"]]);
            let frontmatter = Frontmatter::detect(&book, &[Zone::Header; 2], &Default::default());
            assert_eq!(
                header_publication_date(&Header::default(), &layout, &frontmatter, vec![])
                    .unwrap()
                    .year,
                Some(1984)
            );
        }
    }

    #[test]
    fn reads_jstor_source_year_without_using_download_year() {
        let cover = page_layout(&[&[
            "Source: Comparative Politics, Jan., 1981, Vol. 13",
            "Stable URL: https://www.jstor.org/stable/421620",
            "Accessed: 2016-08-07",
        ]]);
        let frontmatter = Frontmatter::detect(&cover, &[Zone::Headnote; 3], &Default::default());
        assert_eq!(
            header_publication_date(&Header::default(), &cover, &frontmatter, vec![])
                .unwrap()
                .year,
            Some(1981)
        );
    }

    #[test]
    fn filters_venue_watermarks_and_recovers_conference_year() {
        for text in [
            "Portal Books on 2026-11-09 Téléchargé de Scholars Portal Books sur 2026-11-09",
            "OCT 2 5 1999 LIBRARIES A RISK-ADJUSTED PERFORMANCE HISTORY",
            "https://example.org",
            "http://example.org",
            "Stable URL",
            "Accessed in 2016",
            "All rights reserved",
            "Copyright 1981",
        ] {
            assert!(!plausible_venue(text), "{text}");
            let mut header = Header::default();
            apply_venue(
                &mut header,
                BibItem {
                    journal: Some(text.into()),
                    ..Default::default()
                },
            );
            assert!(header.journal.is_none());
        }
        let header = Header {
            meeting: Some("July 10–12, 2023 • Boston, MA, USA".into()),
            ..Default::default()
        };
        assert_eq!(
            header_publication_date(
                &header,
                &Default::default(),
                &Frontmatter::default(),
                vec![]
            )
            .unwrap()
            .year,
            Some(2023)
        );
    }

    #[test]
    fn publication_dates_prefer_publication_context_and_plausible_years() {
        let text = "Received 1999; Revised 2000; Published online: 2001\nDownloaded 2026";
        let ranges = contextual_date_spans(text, 0..text.len());
        let dates = ranges
            .into_iter()
            .filter_map(|(rank, text)| {
                Some((
                    rank,
                    Date {
                        year: Some(year(YEAR.find(text)?.as_str())?),
                        ..Default::default()
                    },
                ))
            })
            .collect();
        assert_eq!(select_publication_date(dates).unwrap().year, Some(2001));
        assert_eq!(
            contextual_date_spans("Accepted: 2020", 10..14),
            [(0, "2020")]
        );
        assert_eq!(year("0406"), None);
        assert_eq!(year("2099"), None);
        assert_eq!(arxiv_date("hep-th/9901001").unwrap().year, Some(1999));
        assert_eq!(arxiv_date("2301.12345").unwrap().month, Some(1));
    }

    #[test]
    fn recovers_a_large_title_before_embedded_metadata() {
        let Some(models) = std::env::var_os("NOSCO_MODELS") else {
            return;
        };
        let engine = Engine::load_for_header(Path::new(&models)).unwrap();
        let mut layout = page_layout(&[&[
            "The work of science",
            "A longer contextual line in a much smaller font",
        ]]);
        layout.tokens[0].font_size = 22.0;
        layout.metadata.title = Some("An unrelated publication title".into());
        let header = engine.process_header_layout(&layout).unwrap();
        assert_eq!(header.title.as_deref(), Some("The work of science"));
    }

    #[test]
    fn assembles_book_and_fallback_titles_with_header_models() {
        let Some(models) = std::env::var_os("NOSCO_MODELS") else {
            return;
        };
        let engine = Engine::load_for_header(Path::new(&models)).unwrap();
        let mut layout = page_layout(&[
            &["A History of Science", "Written by Ada Lovelace"],
            &[
                "Copyright 2021",
                "ISBN 978-0-387-32458-6",
                "ISSN 2413-1954",
                "References",
                "doi: 10.1234/chapter",
            ],
        ]);
        layout.metadata.doi = Some("10.1007/978-0-387-32458-6".into());
        let header = engine.process_header_layout(&layout).unwrap();
        assert_eq!(header.title.as_deref(), Some("A History of Science"));
        assert_eq!(header.authors[0].last_name.as_deref(), Some("Lovelace"));
        assert_eq!(header.isbn.as_deref(), Some("9780387324586"));
        assert_eq!(header.publication_date.unwrap().year, Some(2021));
        assert_eq!(header.doi.as_deref(), Some("10.1007/978-0-387-32458-6"));
        assert_eq!(header.issn.as_deref(), Some("2413-1954"));

        layout.metadata.doi = Some("10.1234/article1".into());
        let header = engine.process_header_layout(&layout).unwrap();
        assert_eq!(header.doi.as_deref(), Some("10.1234/article1"));
        assert!(header.journal.is_none());
        assert!(header.meeting.is_none());

        let mut layout = page_layout(&[&["This page intentionally left blank"]]);
        layout.metadata.title = Some("DISCUSSION PAPER SERIES New findings".into());
        let header = engine.process_header_layout(&layout).unwrap();
        assert_eq!(header.title.as_deref(), Some("New findings"));

        for names in [
            engine.parse_header_names("Ada¹ Lovelace†").unwrap(),
            engine.parse_citation_names("Lovelace†, A.").unwrap(),
        ] {
            assert!(!names.is_empty());
            assert!(
                names
                    .iter()
                    .all(|person| !person.full_name().contains(['¹', '†']))
            );
        }
    }

    #[test]
    fn parses_reference_and_multiline_headnote_with_citation_model() {
        let Some(models) = std::env::var_os("NOSCO_MODELS") else {
            eprintln!("skipping citation model test: set NOSCO_MODELS to a compact pack");
            return;
        };
        let engine = Engine::load_for_header(Path::new(&models)).unwrap();
        for kind in [
            ModelKind::Fulltext,
            ModelKind::Figure,
            ModelKind::Table,
            ModelKind::ReferenceSegmenter,
        ] {
            assert!(engine.model_if_loaded(kind).is_none());
        }
        assert!(engine.model_if_loaded(ModelKind::Citation).is_some());
        let layout = page_layout(&[&[
            "Journal of Biological Chemistry, 2020",
            "Vol. 295, No. 20, pp. 6501–6509",
        ]]);
        let mut segments = Segmented {
            layout: &layout,
            starts: vec![false; 2],
            badge: vec![false; 2],
            furniture: vec![true; 2],
            zones: vec![Zone::Headnote; 2],
            frontmatter: Frontmatter::default(),
            lead_override: Vec::new(),
        };
        let mut header = Header::default();
        let mut scratch = DecodeScratch::new();
        engine
            .parse_header_venue(&segments, &mut header, vec![], &mut scratch)
            .unwrap();
        assert_eq!(
            header.journal.as_deref(),
            Some("Journal of Biological Chemistry")
        );
        assert_eq!(header.volume.as_deref(), Some("295"));
        assert_eq!(header.issue.as_deref(), Some("20"));
        assert_eq!(header.pages.as_deref(), Some("6501–6509"));
        assert_eq!(header.publication_date.unwrap().year, Some(2020));

        segments.zones.fill(Zone::Header);
        let mut header = Header::default();
        engine
            .parse_header_venue(
                &segments,
                &mut header,
                vec![
                    "Nature Communications (2024) 15:1234".into(),
                    "bibliography ".repeat(601),
                ],
                &mut scratch,
            )
            .unwrap();
        assert_eq!(header.journal.as_deref(), Some("Nature Communications"));
        assert_eq!(header.volume.as_deref(), Some("15"));
        assert_eq!(header.pages.as_deref(), Some("1234"));
        assert!(header.note.is_none());
    }

    #[test]
    fn strips_report_numbers_and_total_page_counts_from_nist_imprints() {
        for imprint in [
            "800-63B, 79 pages",
            "800-63C, 49 pages",
            "800-63-3, 74 pages",
        ] {
            let citation = format!("Natl. Inst. Stand. Technol. Spec. Publ. {imprint} (June 2017)");
            assert_eq!(
                nist_report_venue(&citation),
                "Natl. Inst. Stand. Technol. Spec. Publ.  (June 2017)"
            );
        }
        let journal = "Journal of Biological Chemistry, Vol. 295, No. 20, pp. 6501–6509";
        assert_eq!(nist_report_venue(journal), journal);
    }

    #[test]
    fn venue_and_xmp_supply_imprint_fields() {
        let mut header = Header::default();
        apply_venue(
            &mut header,
            BibItem {
                journal: Some("Journal of Examples".to_owned()),
                volume: Some("40".to_owned()),
                issue: Some("2".to_owned()),
                page_range: Some("33-40".to_owned()),
                publisher: Some("Academic Press".to_owned()),
                location: Some("Oxford".to_owned()),
                date: Some(Date {
                    year: Some(2001),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        assert_eq!(header.journal.as_deref(), Some("Journal of Examples"));
        assert_eq!(header.volume.as_deref(), Some("40"));
        assert_eq!(header.issue.as_deref(), Some("2"));
        assert_eq!(header.pages.as_deref(), Some("33-40"));
        assert_eq!(header.publisher.as_deref(), Some("Academic Press"));
        assert_eq!(header.location.as_deref(), Some("Oxford"));
        assert_eq!(header.publication_date.unwrap().year, Some(2001));
        let mut layout = LayoutDocument::default();
        layout
            .metadata
            .xmp
            .insert("prism:coverDate".into(), vec!["1999-07-01".into()]);
        assert_eq!(
            embedded_date(&layout, &["prism:coverDate"]).unwrap().month,
            Some(7)
        );
    }

    #[test]
    fn structured_identifiers_override_weak_frontmatter_matches() {
        let mut layout = page_layout(&[&["DOI: 10.1000/earlier123", "ISSN 1998-2001"]]);
        for (key, value) in [
            ("prism:doi", "10.48550/arXiv.2301.12345"),
            ("prism:eIssn", "2041-1723"),
            ("prism:isbn", "978-0-387-32458-6"),
        ] {
            layout.metadata.xmp.insert(key.into(), vec![value.into()]);
        }
        let frontmatter = Frontmatter::detect(&layout, &[Zone::Header; 2], &Default::default());
        let mut header = Header::default();
        apply_identifiers(&mut header, &layout, &frontmatter, &[], "");
        assert_eq!(header.doi.as_deref(), Some("10.48550/arXiv.2301.12345"));
        assert_eq!(header.arxiv_id.as_deref(), Some("2301.12345"));
        assert_eq!(header.eissn.as_deref(), Some("2041-1723"));
        assert!(header.issn.is_none());
        assert_eq!(header.isbn.as_deref(), Some("9780387324586"));
    }

    #[test]
    fn sparse_orcid_annotations_match_the_nearby_author() {
        let mut authors = [
            Person {
                last_name: Some("First".into()),
                ..Default::default()
            },
            Person {
                last_name: Some("Second".into()),
                ..Default::default()
            },
        ];
        let boxes = [
            ("First".into(), BoundingBox::new(2, 30., 90., 20., 10.)),
            ("Second".into(), BoundingBox::new(2, 80., 90., 25., 10.)),
        ];
        link_orcid_annotations(
            &mut authors,
            &boxes,
            &[(
                BoundingBox::new(2, 108., 90., 8., 8.),
                "0000-0002-1825-0097".into(),
            )],
        );
        assert_eq!(authors[0].orcid, None);
        assert_eq!(authors[1].orcid.as_deref(), Some("0000-0002-1825-0097"));
    }

    #[test]
    fn links_normalized_markers_and_single_unmarked_affiliation() {
        let mut authors = [
            Person {
                affiliation_markers: vec!["[¹], A, ∗".into()],
                ..Default::default()
            },
            Person::default(),
        ];
        let mut affiliations = [
            Affiliation {
                key: Some("aff0".into()),
                marker: Some("1".into()),
                ..Default::default()
            },
            Affiliation {
                key: Some("aff1".into()),
                marker: Some("(a)*".into()),
                ..Default::default()
            },
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
