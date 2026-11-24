use super::{ABSTRACT, REF_HEADING, Zone, page_text_for_identifiers};
use crate::identifiers::*;
use crate::metadata::strip_series_prefix;
use crate::spans::compact;
use crate::text::year;
use nosco_models::{BibItem, Date, Header};
use nosco_pdf::LayoutDocument;
use regex::Regex;
use std::{collections::HashSet, sync::LazyLock};

pub(super) static EDITOR_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:edited by|editors?|herausgegeben von|herausgeber(?:innen)?)\b|\bhg\.|\(\s*eds?\.?\s*\)").unwrap()
});
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
static TITLE_PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:working paper number\s+\d+|working paper series|iab-kurzbericht|this page intentionally left blank|mit architecture|university of copenhagen|online appendix|appendix|supporting information|supplementary (?:materials?|information|data|appendix|file))[.\s]*$").unwrap()
});
static USENIX_SPONSOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bopen access to the proceedings of the\b").unwrap());

static COVER_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|\s)(?:author\(s\)|source|published by|stable url|hal id|to cite this version)\s*:").unwrap()
});
static CMAP_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9'`][A-Z]{2,}|^[A-Z]{2,}(?:[&0-9][A-Z]{2,})+$|(?:[a-z][A-Z]){3}").unwrap()
});
static METADATA_PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:untitled|latex|powerpoint)\b|www\.|(?:document|slide|layout)\s*\d*$|cover(?: sheet| page)?$|full[- ]text$|springer(?: nature)?$|elsevier$|wiley$|taylor & francis$|routledge$|sage publications$|(?:cambridge|oxford) university press$)").unwrap()
});
static BOOK_CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)©|\b(?:copyright|all rights reserved|alle rechte vorbehalten|bibliogra[fp]ische information|deutsche nationalbibliothek|auflage|verlag|printed in|first published)\b").unwrap()
});
static BOOK_YEAR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:©|\b(?:copyright|(?:first )?published|auflage|erschienen|verlag)\b)[^\r\n]{0,80}?\b((?:19|20)\d{2}(?:\s*,\s*(?:19|20)\d{2})*)\b").unwrap()
});
static CONTRIBUTOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^[ \t]*(?:(edited[ \t]+by|editors?|herausgegeben[ \t]+von|herausgeber(?:innen)?|hg)\b|(?:written[ \t]+by|by)\b)[ \t:.]*").unwrap()
});
static EDITION_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:\d+(?:st|nd|rd|th)|first|second|third|fourth|fifth|sixth|revised|new) edition\b|\d+\..*\bauflage\b)").unwrap()
});
static COVER_EDITOR_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+(?:editors?|\(\s*(?:hg|eds?)\.?\s*\))\s*$").unwrap());
static GERMAN_NAME_SEPARATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bund\b").unwrap());
static CHAPTER_DOI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^10\.(?:\d+/978[\d-]+[_./](?:ch)?\d|1093/acrefore/|48550/arxiv\.)").unwrap()
});

static ARTICLE_VENUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:proceedings of\b|(?:the )?(?:international )?journal of\b|(?:journal|conference|meeting)\s*:|vol(?:ume)?\.?\s+\d+.*\b(?:issue|no\.)\s*\d+)").unwrap()
});
static PRODUCTION_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\.(?:xml|indd|qxp)$|^s[0-9]{12,}|^jra\s+\d+\.\.\d+\b|^diw weekly report$)")
        .unwrap()
});

fn is_isbn_doi(id: &str) -> bool {
    id.split_once('/').is_some_and(|(_, suffix)| {
        (suffix.starts_with("978") || suffix.starts_with("979"))
            && suffix.bytes().all(|b| b.is_ascii_digit() || b == b'-')
            && suffix.bytes().filter(u8::is_ascii_digit).count() == 13
    })
}

fn article_evidence(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .any(|line| ABSTRACT.is_match(line) || ARTICLE_VENUE.is_match(line))
}

pub(super) struct BookFrontmatter {
    pub title: String,
    pub half_title_page: usize,
    pub names: Option<(String, bool)>,
    pub date: Option<Date>,
    isbn: String,
    doi: Option<String>,
    pub(super) copyright_page: usize,
}

fn before_references(text: &str) -> &str {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if REF_HEADING.is_match(line.trim()) {
            return &text[..offset];
        }
        offset += line.len();
    }
    text
}

fn cover_editors(text: &str) -> Option<(&str, &str)> {
    let (first, body) = text.split_once('\n')?;
    let marker = COVER_EDITOR_SUFFIX.find(first)?;
    Some((first[..marker.start()].trim(), body))
}

fn book_title_lines(text: &str) -> String {
    compact(
        &text
            .lines()
            .take_while(|line| !EDITION_LINE.is_match(line.trim()))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn book_contributors(text: &str, valid_title: impl FnOnce(&str) -> bool) -> Option<(String, bool)> {
    let marker = CONTRIBUTOR.captures(text)?;
    let matched = marker.get(0)?;
    if !valid_title(&text[..matched.start()]) {
        return None;
    }
    let mut lines = text[matched.end()..].trim_start().lines();
    let mut names = lines.next()?.trim().to_owned();
    for next in lines.map(str::trim) {
        let ends_with_initial = names.split_whitespace().next_back().is_some_and(|word| {
            word.strip_suffix('.').is_some_and(|initial| {
                initial.chars().count() == 1 && initial.starts_with(char::is_uppercase)
            })
        });
        if !names.ends_with(',') && !(ends_with_initial && next.chars().all(char::is_alphabetic)) {
            break;
        }
        names.push(' ');
        names.push_str(next);
    }
    (!names.is_empty()).then_some((names, marker.get(1).is_some()))
}

fn detect_book(pages: &[(usize, String)]) -> Option<BookFrontmatter> {
    let first = pages.iter().position(|(_, text)| !text.trim().is_empty())?;
    let (half_title_page, half_title) = &pages[first];
    if half_title
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(9)
        .count()
        > 8
        || repository_cover(half_title)
        || pages[first..]
            .iter()
            .any(|(_, text)| article_evidence(before_references(text)))
    {
        return None;
    }
    let (cover_names, title_text) = cover_editors(half_title)
        .map_or((None, half_title.as_str()), |(names, body)| {
            (Some((names.to_owned(), true)), body)
        });
    let title = book_title_lines(
        CONTRIBUTOR
            .find(title_text)
            .map_or(title_text, |marker| &title_text[..marker.start()]),
    );
    let title = clean_title(&title);
    if title.len() < 8 || !usable_embedded_title(title) || repeated_title(title) {
        return None;
    }

    if doi(half_title).is_some_and(|id| !is_isbn_doi(&id)) {
        return None;
    }
    let mut names = cover_names.or_else(|| book_contributors(half_title, |_| true));
    for (page, text) in &pages[first + 1..] {
        let local = before_references(text);
        if BOOK_CONTEXT.is_match(local)
            && let Some(isbn) = isbn(local)
        {
            let date = BOOK_YEAR
                .captures_iter(local)
                .flat_map(|parts| parts.get(1).unwrap().as_str().split(','))
                .filter_map(|raw| {
                    let raw = raw.trim();
                    Some(Date {
                        year: Some(year(raw)?),
                        raw: raw.to_owned(),
                        ..Default::default()
                    })
                })
                .max_by_key(|date| date.year);
            let cited_publication = local.lines().position(|line| {
                line.trim()
                    .eq_ignore_ascii_case("Please cite this publication as:")
            });
            let publication_doi = cited_publication
                .and_then(|line| local.lines().skip(line + 1).take(3).find_map(doi));
            let first_doi = doi(local);
            let book_doi = first_doi
                .as_ref()
                .filter(|id| is_isbn_doi(id))
                .cloned()
                .or_else(|| local.lines().filter_map(doi).find(|id| is_isbn_doi(id)))
                .or(publication_doi);
            if first_doi.is_some() && book_doi.is_none() {
                return None;
            }
            return Some(BookFrontmatter {
                title: title.to_owned(),
                half_title_page: *half_title_page,
                names: names.map(|(value, editor)| {
                    (
                        GERMAN_NAME_SEPARATOR.replace_all(&value, ";").into_owned(),
                        editor,
                    )
                }),
                date,
                isbn,
                doi: book_doi,
                copyright_page: *page,
            });
        }
        if local.len() != text.len() || doi(local).is_some_and(|id| !is_isbn_doi(&id)) {
            break;
        }
        if names.is_none() {
            names = book_contributors(local, |lead| {
                let lead = book_title_lines(lead);
                lead.to_lowercase() == title.to_lowercase()
            });
        }
    }
    None
}

#[derive(Default)]
pub(super) struct Frontmatter {
    pages: Vec<(usize, String)>,
    pub(super) candidates: Vec<usize>,
    pub(super) excluded: HashSet<usize>,
    book: bool,
    pub(super) book_metadata: Option<BookFrontmatter>,
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
        let book_metadata = book.then(|| detect_book(&pages)).flatten().filter(|book| {
            xmp(layout, "prism:publicationName").is_none()
                && embedded_dois(layout).filter_map(doi).all(|id| {
                    is_isbn_doi(&id)
                        || (book.doi.as_deref() == Some(&id) && !CHAPTER_DOI.is_match(&id))
                })
        });
        if let Some(book) = &book_metadata {
            excluded.insert(book.copyright_page);
        }
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
            book_metadata,
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

    pub(super) fn edited_title_page(&self) -> Option<(String, String)> {
        let primary = self.primary()?;
        if !self.book {
            return None;
        }
        let text = self.text(primary);
        if article_evidence(text) {
            return None;
        }
        let (editor, _) =
            book_contributors(text, |lead| plausible_title(clean_title(&compact(lead))))
                .filter(|(_, editor)| *editor)?;
        let marker = CONTRIBUTOR.find(text)?;
        Some((
            clean_title(&compact(&text[..marker.start()])).to_owned(),
            editor,
        ))
    }

    pub(super) fn is_book_series_title(&self, title: &str) -> bool {
        let key = |text: &str| {
            text.chars()
                .filter(|ch| ch.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        };
        let title = key(title);
        self.ordered_text().any(|text| {
            let mut lines = text.lines();
            lines.next().is_some_and(|line| key(line) == title)
                && lines.take(3).any(|line| {
                    line.trim()
                        .to_ascii_lowercase()
                        .starts_with("series editor")
                })
        })
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
    strip_series_prefix(text.trim())
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

pub(super) fn usable_embedded_title(text: &str) -> bool {
    text.chars().count() <= 240
        && plausible_title(text)
        && !unresolved_tracked_title(text)
        && !PRODUCTION_TITLE.is_match(text.trim())
}

fn corrupted_title(text: &str) -> bool {
    let mut count = 0;
    let mut cmap = 0;
    for token in text.split_whitespace() {
        count += 1;
        if CMAP_TOKEN.is_match(token) {
            if token.len() >= 10 || (token == text && token.len() >= 6) {
                return true;
            }
            cmap += 1;
        }
    }
    cmap >= 2 && cmap * 5 >= count * 2
}

pub(super) fn unresolved_tracked_title(text: &str) -> bool {
    let mut count = 0;
    let mut singles = 0;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        if len > 2 || !word.chars().all(char::is_uppercase) {
            return false;
        }
        count += 1;
        singles += usize::from(len == 1);
    }
    count >= 5 && singles * 2 >= count
}

pub(super) fn repeated_title(text: &str) -> bool {
    let words = || {
        text.split(|ch: char| !ch.is_alphanumeric())
            .filter(|word| !word.is_empty())
    };
    let count = words().count();
    count >= 4
        && count % 2 == 0
        && words()
            .take(count / 2)
            .zip(words().skip(count / 2))
            .all(|(left, right)| {
                left.eq_ignore_ascii_case(right)
                    || ((!left.is_ascii() || !right.is_ascii())
                        && left
                            .chars()
                            .flat_map(char::to_lowercase)
                            .eq(right.chars().flat_map(char::to_lowercase)))
            })
}

pub(super) fn plausible_title(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    (3..=340).contains(&text.chars().count())
        && text.chars().any(char::is_alphabetic)
        && !NUMBERED_SECTION.is_match(text.trim())
        && !TITLE_PLACEHOLDER.is_match(text.trim())
        && !METADATA_PLACEHOLDER.is_match(text.trim())
        && !text.contains('�')
        && !COVER_LABEL.is_match(text)
        && !corrupted_title(text)
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
        && ![".pdf", ".docx", ".doc", ".tex", ".dvi", ".indd"]
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

fn embedded_dois(layout: &LayoutDocument) -> impl Iterator<Item = &str> + Clone {
    let metadata = &layout.metadata;
    ["prism:doi", "pdfx:doi", "crossmark:DOI", "dc:identifier"]
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
        )
}

pub(super) fn apply_identifiers(
    header: &mut Header,
    layout: &LayoutDocument,
    frontmatter: &Frontmatter,
    links: &[nosco_pdf::PdfLink],
    raw: &str,
) {
    let metadata = &layout.metadata;
    let embedded = embedded_dois(layout);
    let texts = || std::iter::once(raw).chain(frontmatter.ordered_text());
    if let Some(book) = &frontmatter.book_metadata {
        header.isbn = Some(book.isbn.clone());
        header.doi = book.doi.clone().or_else(|| {
            embedded
                .clone()
                .filter_map(doi)
                .chain(
                    links
                        .iter()
                        .filter(|link| link.page == book.copyright_page)
                        .filter_map(|link| doi(&link.uri)),
                )
                .find(|id| is_isbn_doi(id))
        });
    } else {
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
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_embedded_production_titles() {
        for title in [
            "ANRV312-PL10-01.xml",
            "Article Proof.indd",
            "Article Proof.qxp",
            "S1469356916000057jra 68..106",
            "jra 68..106",
            "DIW Weekly Report",
        ] {
            assert!(!usable_embedded_title(title), "{title}");
        }
        assert!(usable_embedded_title(
            "A New History of Scientific Publishing"
        ));
    }

    #[test]
    fn vetoes_books_with_article_evidence() {
        for line in [
            "Abstract: The study begins here.",
            "Journal of Example Research",
            "Proceedings of the Example Conference",
            "DOI 10.1234/article1",
        ] {
            let pages = [
                (1, format!("An English Book Title\n{line}")),
                (2, "Copyright 2001\nISBN 978-0-387-32458-6".into()),
            ];
            assert!(detect_book(&pages).is_none(), "{line}");
        }
        let copyright = "Copyright 2001\nISBN 978-0-387-32458-6";
        let pages = [
            (1, "A Sparse Title Page".into()),
            (2, format!("{copyright}\nDOI 10.1234/article1")),
        ];
        assert!(detect_book(&pages).is_none());
        let pages = [
            (1, "A Sparse Title Page".into()),
            (2, copyright.into()),
            (3, "Abstract: The study begins here.".into()),
        ];
        assert!(detect_book(&pages).is_none());
    }

    #[test]
    fn recognizes_editor_labels_and_the_most_recent_edition() {
        for label in [
            "Editors",
            "Edited by",
            "Hg.",
            "Herausgegeben von",
            "Herausgeber",
        ] {
            let pages = [
                (1, format!("An English Book Title\n{label}\nAda Lovelace")),
                (
                    2,
                    "Copyright 1995\n6. Auflage 2019\nISBN 978-0-387-32458-6".into(),
                ),
            ];
            let book = detect_book(&pages).unwrap();
            assert_eq!(book.names, Some(("Ada Lovelace".into(), true)), "{label}");
            assert_eq!(book.date.unwrap().year, Some(2019));
        }
    }

    #[test]
    fn finds_german_book_metadata_after_the_half_title() {
        let pages = [
            (1, String::new()),
            (2, "Eine Geschichte des Buches".into()),
            (3, "Eine Geschichte des Buches\nHerausgegeben von\nAnna Müller und Johann Bach".into()),
            (4, "Bibliografische Information der Deutschen Nationalbibliothek\nErschienen 2021 im Beispiel Verlag\nISBN 978-0-387-32458-6\nDOI 10.1007/978-0-387-32458-6_2\nDOI 10.1007/978-0-387-32458-6".into()),
            (5, "Contents\nAnother DOI: 10.1234/article1".into()),
        ];
        let book = detect_book(&pages).unwrap();
        assert_eq!(book.title, "Eine Geschichte des Buches");
        assert_eq!(book.names, Some(("Anna Müller ; Johann Bach".into(), true)));
        assert_eq!(book.isbn, "9780387324586");
        assert_eq!(book.date.unwrap().year, Some(2021));
        assert_eq!(book.doi.as_deref(), Some("10.1007/978-0-387-32458-6"));
    }

    #[test]
    fn keeps_book_identifiers_out_of_the_bibliography() {
        let mut pages = [
            (1, "An English Book Title\nWritten by Ada Lovelace".into()),
            (2, "First published in 2001\nAll rights reserved\nISBN 978-0-387-32458-6\nBibliography\nDOI 10.1234/unrelated".into()),
        ];
        let book = detect_book(&pages).unwrap();
        assert_eq!(book.names, Some(("Ada Lovelace".into(), false)));
        assert_eq!(book.date.unwrap().year, Some(2001));
        assert!(book.doi.is_none());
        pages[1].1 = "Copyright 2001\nReferences\nISBN 978-0-387-32458-6".into();
        assert!(detect_book(&pages).is_none());
        pages[0].1 = "A very long first page\n".repeat(9);
        pages[1].1 = "Copyright 2001\nISBN 978-0-387-32458-6".into();
        assert!(detect_book(&pages).is_none());
    }

    #[test]
    fn rejects_corruption_cover_labels_repetition_and_watermarks() {
        for title in [
            "1HROLEHUDOLVP",
            "SuPpLeMeNt",
            "Broken � Title",
            "This page intentionally left blank.",
            "Working Paper Number 27",
            "MIT Architecture",
            "University of Copenhagen",
            "2. Method",
            "1. Introduction",
            "Online Appendix",
            "Supplementary Information",
            "Author(s): Jane Smith",
            "Legitimate Title Source: Example",
            "Legitimate Title Published by: Example",
            "Stable URL: Example",
            "HAL Id: Example",
            "To cite this version: Example",
        ] {
            assert!(!plausible_title(title), "{title}");
        }
        assert!(repeated_title(
            "Weaponized Interdependence — Weaponized Interdependence"
        ));
        assert!(!repeated_title(
            "Weaponized Interdependence and New Perspectives"
        ));
        for venue in [
            "Portal Books",
            "Scholars Portal",
            "Téléchargé",
            "Downloaded from",
            "LIBRARIES",
            "Stable URL",
        ] {
            assert!(!plausible_venue(venue), "{venue}");
        }
    }
}
