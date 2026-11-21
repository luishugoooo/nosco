use super::{ABSTRACT, Zone, page_text_for_identifiers};
use crate::identifiers::*;
use crate::spans::compact;
use crate::text::year;
use nosco_models::{BibItem, Date, Header};
use nosco_pdf::LayoutDocument;
use regex::Regex;
use std::{collections::HashSet, sync::LazyLock};

pub(super) static EDITOR_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:edited by|editors?)\b|\(\s*eds?\.?\s*\)").unwrap());
static EDITED_BY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^[ \t]*edited[ \t]+by\b[ \t:]*").unwrap());
static DATE_CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(published|online|issue|available|received|revised|accepted|downloaded|accessed|submitted)\b").unwrap()
});
static COPYRIGHT_YEAR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:©|\bcopyright\b|\(c\))[ \t]*(?:©[ \t]*)?(?:\r?\n[ \t]*)?([0-9]{4})\b")
        .unwrap()
});

static ACCESS_NOTICE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:accessed|downloaded|téléchargé\p{L}*|retrieved|printed|stable\s+url|all\s+content\s+following\s+this\s+page|scholars\s+portal)\b").unwrap()
});
static VENUE_BOILERPLATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)https?://|\b(?:portal\s+books|libraries|all\s+rights\s+reserved|copyright)\b")
        .unwrap()
});
pub(super) static NUMBERED_SECTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:\d{1,2}(?:\.\d+)+\.?\s+|\d{1,2}\.\s+|\d+(?:\.\d+)*\.?\s+(?:introduction|background|methods?|methodology|related work|experiments|results|discussion|conclusions?|abstract|overview|preliminaries)\b)").unwrap()
});
static SERIES_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:(?:nber\s+)?working paper series|(?:iza\s+)?discussion paper series)\b[\s:–—-]*",
    )
    .unwrap()
});
static TITLE_PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:working paper number\s+\d+|iab-kurzbericht|this page intentionally left blank|online appendix|supplementary material)[.\s]*$").unwrap()
});
static USENIX_SPONSOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bopen access to the proceedings of the\b").unwrap());

#[derive(Default)]
pub(super) struct Frontmatter {
    pages: Vec<(usize, String)>,
    pub(super) candidates: Vec<usize>,
    pub(super) excluded: HashSet<usize>,
    book: bool,
}

impl Frontmatter {
    pub(super) fn detect(
        layout: &LayoutDocument,
        zones: &[Zone],
        lexicon: &nosco_lexicon::Lexicon,
    ) -> Self {
        let pages = layout
            .pages
            .iter()
            .filter(|page| page.number <= 8)
            .map(|page| {
                (
                    page.number,
                    page_text_for_identifiers(layout, page.number, lexicon),
                )
            })
            .collect::<Vec<_>>();
        let mut excluded = HashSet::new();
        for (number, text) in &pages {
            if text.trim().is_empty()
                || (*number <= 2 && repository_cover(text))
                || non_title_page(text)
            {
                excluded.insert(*number);
            }
        }

        // Sparse half-titles and series leaves precede the title page in books.
        // Require evidence of a book or a richer next page with a repeated title.
        let book = pages.iter().any(|(_, text)| isbn(text).is_some());
        let leading = pages
            .iter()
            .filter(|(page, _)| *page <= 6 && !excluded.contains(page))
            .collect::<Vec<_>>();
        for pair in leading.windows(2).take(3) {
            let [(number, text), (next_number, next)] = pair else {
                continue;
            };
            let word_count = text.split_whitespace().count();
            let repeated = compact(next)
                .to_lowercase()
                .contains(&compact(text).to_lowercase());
            let has_header = layout
                .pages
                .iter()
                .find(|page| page.number == *next_number)
                .is_some_and(|page| page.line_range.clone().any(|i| zones[i] == Zone::Header));
            let next_has_editor = EDITOR_LABEL.is_match(next);
            if word_count <= 25
                && next.split_whitespace().count() > word_count
                && ((book && (has_header || repeated || next_has_editor))
                    || (repeated && has_header))
            {
                excluded.insert(*number);
            }
        }

        let candidates = pages
            .iter()
            .filter_map(|(number, _)| {
                (*number <= 6 && !excluded.contains(number)).then_some(*number)
            })
            .collect();
        Self {
            pages,
            candidates,
            excluded,
            book,
        }
    }

    pub(super) fn primary(&self) -> Option<usize> {
        self.candidates.first().copied()
    }

    pub(super) fn text(&self, page: usize) -> &str {
        self.pages
            .iter()
            .find(|(number, _)| *number == page)
            .map_or("", |(_, text)| text)
    }

    pub(super) fn edited_title_page(&self) -> Option<(String, &str)> {
        let primary = self.primary()?;
        if !self.book {
            return None;
        }
        let text = self.text(primary);
        let marker = EDITED_BY.find(text)?;
        let title = compact(&text[..marker.start()]);
        let title = clean_title(&title).to_owned();
        let editor = text[marker.end()..].trim_start().lines().next()?.trim();
        (plausible_title(&title) && !editor.is_empty()).then_some((title, editor))
    }

    pub(super) fn ordered_text(&self) -> impl Iterator<Item = &str> {
        self.primary()
            .into_iter()
            .map(|page| self.text(page))
            .chain(
                self.pages
                    .iter()
                    .filter(|(page, _)| Some(*page) != self.primary())
                    .map(|(_, text)| text.as_str()),
            )
    }
}

pub(super) fn repository_cover(text: &str) -> bool {
    let lead = compact(&text.lines().take(12).collect::<Vec<_>>().join(" ")).to_lowercase();
    let text = compact(text).to_lowercase();
    [
        "your use of the jstor archive",
        "hal is a multi-disciplinary open access archive",
        "see discussions, stats, and author profiles",
        "this is the author's version",
        "this is the author’s version",
        "downloaded from the institutional repository",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
        || (lead.contains("electronic copy available at") && lead.contains("ssrn"))
        || (text.contains("hal id:") && text.contains("submitted on"))
        || (text.contains("stable url:")
            && (text.contains("author(s):") || text.contains("published by:")))
        || (text.contains("to cite this article")
            && (text.contains("tandfonline") || text.contains("link to this article")))
        || ([
            "sagepub",
            "oxford academic",
            "university repository",
            "researchgate",
            "ssrn",
        ]
        .iter()
        .any(|site| text.contains(site))
            && [
                "downloaded from",
                "to cite this",
                "accepted manuscript",
                "repository copy",
            ]
            .iter()
            .any(|phrase| text.contains(phrase)))
}

pub(super) fn access_notice(text: &str) -> bool {
    ACCESS_NOTICE.is_match(text)
}

fn download_watermark(text: &str) -> bool {
    let flat = compact(text).to_lowercase();
    [
        "jstor is a not-for-profit",
        "stable url",
        "downloaded from",
        "scholars portal",
        "all content following this page",
    ]
    .iter()
    .any(|marker| flat.contains(marker))
        || (access_notice(&flat)
            && [
                "jstor",
                "researchgate",
                "proquest",
                "ebsco",
                "cambridge core",
            ]
            .iter()
            .any(|provider| flat.contains(provider)))
        || repository_cover(text)
}

pub(super) fn plausible_venue(text: &str) -> bool {
    !text.trim().is_empty() && !access_notice(text) && !VENUE_BOILERPLATE.is_match(text)
}

pub(super) fn clean_title(text: &str) -> &str {
    let text = USENIX_SPONSOR
        .find(text)
        .map_or(text, |marker| &text[..marker.start()]);
    let text = text.trim();
    SERIES_PREFIX
        .find(text)
        .map_or(text, |prefix| &text[prefix.end()..])
}

pub(super) fn repairs_uppercase_spacing(title: &str, embedded: &str) -> bool {
    title.chars().any(char::is_uppercase)
        && !title.chars().any(char::is_lowercase)
        && plausible_title(embedded)
        && embedded.split_whitespace().count() < title.split_whitespace().count()
        && title
            .split_whitespace()
            .collect::<String>()
            .eq_ignore_ascii_case(&embedded.split_whitespace().collect::<String>())
}

pub(super) fn restore_tracked_title(text: String, is_word: impl Fn(&str) -> bool) -> String {
    let words: Vec<_> = text.split_whitespace().collect();
    if words.len() < 6
        || words
            .iter()
            .any(|word| word.len() > 2 || !word.bytes().all(|c| c.is_ascii_uppercase()))
        || words.iter().filter(|word| word.len() == 1).count() <= words.len() / 2
    {
        return text;
    }

    // Prefer the fewest dictionary words, and leave ties untouched.
    let joined = words.concat();
    let mut splits = vec![(usize::MAX, None); joined.len() + 1];
    splits[joined.len()] = (0, Some(joined.len()));
    for start in (0..joined.len()).rev() {
        for end in start + 3..=joined.len() {
            let (remaining, next) = splits[end];
            if remaining == usize::MAX || !is_word(&joined[start..end]) {
                continue;
            }
            let count = remaining + 1;
            if count < splits[start].0 {
                splits[start] = (count, next.map(|_| end));
            } else if count == splits[start].0 {
                splits[start].1 = None;
            }
        }
    }
    let mut restored = Vec::new();
    let mut start = 0;
    while start < joined.len() {
        let Some(end) = splits[start].1 else {
            return text;
        };
        restored.push(&joined[start..end]);
        start = end;
    }
    restored.join(" ")
}

fn non_title_page(text: &str) -> bool {
    let lead = text.lines().take(5).map(str::trim);
    if lead.clone().any(|line| {
        matches!(
            line.to_ascii_lowercase().as_str(),
            "table of contents"
                | "contents"
                | "contributors"
                | "list of contributors"
                | "preface"
                | "foreword"
                | "list of illustrations"
        )
    }) {
        return true;
    }
    if EDITED_BY.find_iter(text).take(3).count() == 3
        || (lead
            .clone()
            .any(|line| line.to_ascii_lowercase().starts_with("series editor"))
            && EDITOR_LABEL.find_iter(text).take(3).count() == 3)
    {
        return true;
    }
    let flat = compact(text).to_lowercase();
    [
        "all rights reserved",
        "cataloging-in-publication",
        "cataloguing-in-publication",
        "printed on acid-free paper",
        "no part of this publication",
    ]
    .iter()
    .any(|phrase| flat.contains(phrase))
        && (flat.contains("cataloging-in-publication")
            || flat.contains("cataloguing-in-publication")
            || isbn(text).is_some()
            || text.split_whitespace().count() < 100)
        && !text.lines().any(|line| ABSTRACT.is_match(line))
}

pub(super) fn boilerplate(text: &str) -> bool {
    let text = compact(text).to_lowercase();
    [
        "all rights reserved",
        "may not be translated or copied",
        "without the written permission",
        "downloaded from",
        "terms of use",
        "terms and conditions",
        "cataloging-in-publication",
        "cataloguing-in-publication",
        "printed on acid-free paper",
        "no part of this publication",
        "your use of the jstor archive",
        "copyright holder",
        "creative commons license",
        "except for brief excerpts",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
}

pub(super) fn plausible_title(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    (3..=340).contains(&text.chars().count())
        && text.chars().any(char::is_alphabetic)
        && !NUMBERED_SECTION.is_match(text.trim())
        && !TITLE_PLACEHOLDER.is_match(text.trim())
        && !clean_title(text).is_empty()
        && !boilerplate(text)
        && !repository_cover(text)
        && !matches!(
            lower.as_str(),
            "untitled" | "layout 1" | "title" | "contents" | "table of contents" | "contributors"
        )
        && ![
            "microsoft word",
            "doi:",
            "pii:",
            "http:",
            "https:",
            "isbn",
            "arxiv:",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        && ![".pdf", ".docx", ".doc", ".tex", ".indd"]
            .iter()
            .any(|suffix| lower.ends_with(suffix))
        && lower.matches(". ").count() <= 3
}

pub(super) fn plausible_metadata_author(text: &str) -> bool {
    plausible_title(text)
        && !matches!(
            text.trim().to_lowercase().as_str(),
            "author" | "administrator" | "unknown" | "user" | "adobe" | "default"
        )
}

pub(super) fn plausible_abstract(text: &str) -> bool {
    (25..=5000).contains(&text.chars().count())
        && !text.starts_with([')', ']', '©'])
        && !boilerplate(text)
}

pub(super) fn xmp<'a>(layout: &'a LayoutDocument, key: &str) -> Option<&'a str> {
    layout.metadata.xmp.get(key)?.first().map(String::as_str)
}

pub(super) fn apply_identifiers(
    header: &mut Header,
    layout: &LayoutDocument,
    frontmatter: &Frontmatter,
    links: &[nosco_pdf::PdfLink],
    raw: &str,
) {
    let metadata = &layout.metadata;
    let embedded = ["prism:doi", "pdfx:doi", "crossmark:DOI", "dc:identifier"]
        .into_iter()
        .filter_map(|key| metadata.xmp.get(key))
        .flatten()
        .map(String::as_str)
        .chain(
            [
                metadata.doi.as_deref(),
                metadata.subject.as_deref(),
                metadata.keywords.as_deref(),
                metadata.title.as_deref(),
            ]
            .into_iter()
            .flatten(),
        );
    let texts = || std::iter::once(raw).chain(frontmatter.ordered_text());
    header.doi = embedded
        .clone()
        .find_map(doi)
        .or_else(|| {
            let primary_links =
                links.partition_point(|link| Some(link.page) == frontmatter.primary());
            article_doi_from_links(&links[..primary_links])
                .or_else(|| {
                    doi(&[
                        raw,
                        frontmatter
                            .primary()
                            .map_or("", |page| frontmatter.text(page)),
                    ]
                    .join("\n\n"))
                })
                .or_else(|| article_doi_from_links(&links[primary_links..]))
        })
        .or_else(|| doi(&texts().collect::<Vec<_>>().join("\n\n")));
    header.arxiv_id = header
        .doi
        .as_deref()
        .and_then(arxiv)
        .or_else(|| arxiv_from_links(links))
        .or_else(|| {
            metadata
                .arxiv
                .as_deref()
                .and_then(|id| arxiv(&format!("arXiv:{id}")))
        })
        .or_else(|| embedded.clone().find_map(arxiv))
        .or_else(|| {
            frontmatter
                .primary()
                .filter(|page| *page <= 3)
                .into_iter()
                .chain(1..=3)
                .find_map(|page| arxiv(frontmatter.text(page)))
        });
    header.pmid = texts().find_map(|text| capture(&PMID, text));
    header.pmcid = texts()
        .find_map(|text| capture(&PMCID, text))
        .map(|value| value.to_ascii_uppercase());
    let isbn_metadata = [xmp(layout, "prism:isbn"), metadata.isbn.as_deref()]
        .into_iter()
        .flatten()
        .map(|value| format!("ISBN {value}"));
    let copyright_pages = frontmatter
        .pages
        .iter()
        .map(|(_, text)| text.as_str())
        .filter(|text| COPYRIGHT_YEAR.is_match(text) || non_title_page(text))
        .collect::<Vec<_>>();
    let structured_isbn = isbn_metadata
        .chain(embedded.clone().map(str::to_owned))
        .collect::<Vec<_>>();
    let structured = isbn(&structured_isbn.join("\n"));
    let copyright = isbn(&copyright_pages.join("\n"));
    header.isbn = structured
        .filter(|value| {
            value.len() == 13 || copyright.as_ref().is_none_or(|other| other.len() != 13)
        })
        .or(copyright)
        .or_else(|| isbn(&texts().collect::<Vec<_>>().join("\n")));
    for (target, electronic, key) in [
        (&mut header.issn, false, "prism:issn"),
        (&mut header.eissn, true, "prism:eIssn"),
    ] {
        let value = xmp(layout, key)
            .or_else(|| (!electronic).then_some(metadata.issn.as_deref()).flatten());
        *target = value
            .and_then(|value| {
                issn(
                    &format!("{}ISSN {value}", if electronic { "e" } else { "" }),
                    electronic,
                )
            })
            .or_else(|| embedded.clone().find_map(|text| issn(text, electronic)))
            .or_else(|| texts().find_map(|text| issn(text, electronic)));
    }
    for url in urls(raw).chain(
        frontmatter
            .ordered_text()
            .flat_map(urls)
            .chain(links.iter().map(|link| link.uri.as_str()))
            .filter(|url| {
                [
                    "jstor.org/stable/",
                    "ssrn.com/abstract=",
                    "hal.science/hal-",
                ]
                .iter()
                .any(|prefix| url.to_lowercase().contains(prefix))
            }),
    ) {
        if !header.urls.iter().any(|other| other == url) {
            header.urls.push(url.to_owned());
        }
    }
}

pub(super) fn apply_venue(header: &mut Header, item: BibItem) {
    header.journal = header
        .journal
        .take()
        .or(item.journal.filter(|journal| plausible_venue(journal)));
    header.volume = header.volume.take().or(item.volume);
    header.issue = header.issue.take().or(item.issue);
    header.pages = header.pages.take().or(item.page_range);
    header.publisher = header.publisher.take().or(item.publisher);
    header.location = header.location.take().or(item.location);
    if header.publication_date.is_none() {
        header.publication_date = item.date.filter(|date| date.year.is_some());
    }
}

pub(super) fn apply_xmp_imprint(header: &mut Header, layout: &LayoutDocument) {
    for (target, field) in [
        (&mut header.journal, "prism:publicationName"),
        (&mut header.volume, "prism:volume"),
        (&mut header.issue, "prism:number"),
        (&mut header.publisher, "dc:publisher"),
        (&mut header.language, "dc:language"),
    ] {
        if target.is_none() {
            *target = xmp(layout, field).map(str::to_owned);
        }
    }
    if header.pages.is_none() {
        header.pages = xmp(layout, "prism:startingPage").map(|start| {
            xmp(layout, "prism:endingPage")
                .filter(|end| *end != start)
                .map_or_else(|| start.to_owned(), |end| format!("{start}-{end}"))
        });
    }
}

pub(super) fn contextual_date_spans(text: &str, range: std::ops::Range<usize>) -> Vec<(u8, &str)> {
    let mut start = range.start;
    let mut spans = Vec::new();
    for value in text[range.clone()].split_inclusive('\n') {
        let end = start + value.trim_end_matches('\n').len();
        let line_start = text[..start].rfind('\n').map_or(0, |offset| offset + 1);
        let line_end = end + text[end..].find('\n').unwrap_or(text.len() - end);
        if !access_notice(&text[line_start..line_end]) {
            spans.extend(rank_date_span(text, start..end));
        }
        start += value.len();
    }
    spans
}

fn rank_date_span(text: &str, range: std::ops::Range<usize>) -> Vec<(u8, &str)> {
    let previous = &text[..range.start];
    let last_line = previous.rsplit('\n').next().unwrap_or_default();
    let context = if last_line.trim().is_empty() {
        let preceding_line = previous.trim_end().rsplit('\n').next().unwrap_or_default();
        if access_notice(preceding_line) && !YEAR.is_match(preceding_line) {
            return Vec::new();
        }
        if YEAR.is_match(preceding_line) {
            ""
        } else {
            preceding_line
        }
    } else {
        last_line
    };
    let mut rank = DATE_CONTEXT
        .find_iter(context)
        .last()
        .filter(|m| context[m.end()..].split_whitespace().count() <= 7)
        .map_or(1, |m| date_rank(m.as_str()));
    let mut value = &text[range];
    let mut spans = Vec::new();
    while let Some(marker) = DATE_CONTEXT.find(value) {
        if marker.start() > 0 && !value[..marker.start()].trim().is_empty() {
            spans.push((rank, &value[..marker.start()]));
        }
        rank = date_rank(marker.as_str());
        value = &value[marker.end()..];
    }
    if !value.trim().is_empty() {
        spans.push((rank, value));
    }
    spans
}

fn date_rank(marker: &str) -> u8 {
    match marker.to_ascii_lowercase().as_str() {
        "published" | "available" | "online" | "issue" => 2,
        _ => 0,
    }
}

pub(super) fn select_publication_date(candidates: Vec<(u8, Date)>) -> Option<Date> {
    use chrono::Datelike;
    let today = chrono::Utc::now().date_naive();
    candidates
        .into_iter()
        .filter(|(rank, date)| {
            *rank > 0
                && date.year.is_some_and(|year| {
                    (year, date.month.unwrap_or(1), date.day.unwrap_or(1))
                        <= (today.year(), today.month() as u8, today.day() as u8)
                })
        })
        .max_by_key(|(rank, date)| (*rank, date.year, date.month, date.day))
        .map(|(_, date)| date)
}

fn jstor_source_date(text: &str) -> Option<Date> {
    let flat = compact(text).to_ascii_lowercase();
    if !flat.contains("jstor")
        || !(flat.contains("stable url") || flat.contains("your use of the jstor archive"))
    {
        return None;
    }
    text.lines()
        .map(str::trim_start)
        .filter(|line| !access_notice(line))
        .filter_map(|line| {
            let (label, source) = line.split_once(':')?;
            label
                .trim()
                .eq_ignore_ascii_case("source")
                .then_some(source)
        })
        .find_map(|source| {
            let raw = YEAR
                .find_iter(source)
                .find(|value| year(value.as_str()).is_some())?
                .as_str();
            Some(Date {
                year: year(raw),
                raw: raw.to_owned(),
                ..Default::default()
            })
        })
}

pub(super) fn header_publication_date(
    header: &Header,
    layout: &LayoutDocument,
    frontmatter: &Frontmatter,
    candidates: Vec<(u8, Date)>,
) -> Option<Date> {
    select_publication_date(
        candidates
            .iter()
            .filter(|(rank, _)| *rank > 1)
            .cloned()
            .collect(),
    )
    .or_else(|| {
        copyright_date(
            header
                .copyright
                .iter()
                .map(String::as_str)
                .chain(frontmatter.ordered_text()),
        )
    })
    .or_else(|| frontmatter.ordered_text().find_map(jstor_source_date))
    .or_else(|| select_publication_date(candidates))
    .or_else(|| header.publication_date.clone())
    .or_else(|| {
        embedded_date(
            layout,
            &["prism:publicationDate", "prism:coverDate", "dc:date"],
        )
    })
    .or_else(|| header.arxiv_id.as_deref().and_then(arxiv_date))
    .or_else(|| {
        [&header.meeting, &header.journal]
            .into_iter()
            .flatten()
            .filter(|venue| plausible_venue(venue))
            .find_map(|venue| {
                let raw = YEAR
                    .find_iter(venue)
                    .find(|value| year(value.as_str()).is_some())?
                    .as_str();
                Some(Date {
                    year: year(raw),
                    raw: raw.to_owned(),
                    ..Default::default()
                })
            })
    })
    .or_else(|| {
        // Creation dates describe the PDF file and are especially unreliable for archive copies.
        if frontmatter.ordered_text().any(download_watermark) {
            return None;
        }
        embedded_date(layout, &["xmp:CreateDate"])
    })
}

pub(super) fn copyright_date<'a>(texts: impl Iterator<Item = &'a str>) -> Option<Date> {
    texts
        .flat_map(|text| {
            COPYRIGHT_YEAR.captures_iter(text).filter(move |cap| {
                let matched = cap.get(0).unwrap();
                let before = text[..matched.start()]
                    .rsplit('\n')
                    .next()
                    .unwrap_or_default();
                let after = text[matched.end()..].split('\n').next().unwrap_or_default();
                !access_notice(before) && !access_notice(after)
            })
        })
        .find_map(|cap| {
            Some(Date {
                year: Some(year(&cap[1])?),
                raw: cap[0].to_owned(),
                ..Default::default()
            })
        })
}

pub(super) fn embedded_date(layout: &LayoutDocument, keys: &[&str]) -> Option<Date> {
    keys.iter()
        .filter_map(|key| xmp(layout, key))
        .find_map(|value| {
            let raw = value.trim();
            let parts: Vec<_> = raw.split('T').next()?.split('-').collect();
            Some(Date {
                year: Some(year(parts[0])?),
                month: parts
                    .get(1)
                    .and_then(|month| month.parse().ok())
                    .filter(|month| (1..=12).contains(month)),
                day: parts
                    .get(2)
                    .and_then(|day| day.parse().ok())
                    .filter(|day| (1..=31).contains(day)),
                raw: raw.to_owned(),
            })
        })
}

pub(super) fn arxiv_date(id: &str) -> Option<Date> {
    let prefix = id.rsplit('/').next()?.get(..4)?;
    let short_year = prefix[..2].parse::<i32>().ok()?;
    let publication_year = if id.contains('/') && short_year >= 91 {
        1900 + short_year
    } else {
        2000 + short_year
    };
    Some(Date {
        year: Some(year(&publication_year.to_string())?),
        month: prefix[2..].parse().ok(),
        raw: format!("arXiv:{id}"),
        ..Default::default()
    })
}
