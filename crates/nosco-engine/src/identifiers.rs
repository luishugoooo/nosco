use std::sync::LazyLock;

use regex::Regex;

macro_rules! pattern {
    ($name:ident, $value:literal) => {
        pub(crate) static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($value).expect("valid scholarly identifier pattern"));
    };
}

pattern!(DOI, r"(?i)\b10\.[0-9]{4,9}[ \t]*/[ \t]*[^\s<>\x22©|?#]+");
pattern!(
    DOI_WRAPPED,
    r"(?i)\b(10\.[0-9]{4,9}[ \t]*/[ \t]*(?:[^\s<>\x22©|?#]*[./_-])?)[ \t]*\r?\n[ \t]*([a-z0-9][^\s<>\x22:©|?#]{1,64})"
);
pattern!(SUB_ASSET_DOI, r"(?i)\.(?:g|t|s)\d{3,}$");
pattern!(
    ARXIV,
    r"(?i)(?:\b10\.48550/arxiv\.|\barxiv\s*(?::\s*|\s+(?:preprint\s+)?)|arxiv\.org/(?:abs|pdf|html|e-print)/)([0-9]{4}[ \t]*\.[ \t]*[0-9]{4,5}(?:v[0-9]+)?|[a-z][a-z.\-]*/[0-9]{7}(?:v[0-9]+)?)\b"
);
pattern!(
    PMID,
    r"(?i)(?:\bpmid\s*[:=]?\s*|pubmed\.ncbi\.nlm\.nih\.gov/|ncbi\.nlm\.nih\.gov/pubmed/)([0-9]{1,9})\b"
);
pattern!(PMCID, r"(?i)\b(PMC[0-9]{4,9})\b");
pattern!(
    ISSN,
    r"(?i)\b(e[ \t]*[-–—−]?[ \t]*|online[ \t]+|electronic[ \t]+|p[- ]?|print[ \t]+)?issn\s*[:=]?\s*([0-9]{4}[ \t]*[-–—−]?[ \t]*[0-9]{3}[0-9x])\b"
);
// Bind each candidate to its own ISBN label so a neighboring control number
// cannot be accepted solely because its checksum happens to match.
pattern!(
    ISBN,
    r"(?i)\b(?:e[ \t]*[-–—−]?[ \t]*)?isbn(?:[ \t]*[-–—−]?[ \t]*(1[03])\b)?(?:[ \t]*\([^\r\n)]{1,25}\))?[ \t]*[:=]?[ \t]*(?:\r?\n[ \t]*)?([0-9](?:[ \t\-–—−‐]*[0-9x]){9,12})\b"
);
pattern!(URL, r"(?i)\b(?:https?://|www\.)[^\s<>\x22]+");
pattern!(EMAIL, r"(?i)[a-z0-9.!#$%&'*+/=?^_`|~-]+@[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)*\.[a-z]{2,}\b");
pattern!(GROUP_EMAIL, r"(?i)\{([^{}@]+)\}@([a-z0-9-]+(?:\.[a-z0-9-]+)+)");
pattern!(ORCID, r"(?i)\b(?:https?://orcid\.org/|ORCID\s*:?\s*)?(\d{4}-\d{4}-\d{4}-\d{3}[\dX])\b");
pattern!(ATTACHED_NAME_MARKER, r"(?i)[⁰¹²³⁴⁵⁶⁷⁸⁹*∗†‡§¶′″]+");
pattern!(AFFILIATION_LINE_MARKER, r"(?m)^\s*([⁰¹²³⁴⁵⁶⁷⁸⁹*∗†‡§¶]+|\[\d+\]|\d{1,2}[.)]?|[a-z][.)])\s+");
pattern!(NAME_SEPARATOR, r"(?i)\bet\s+al\b\.?|\band\b|[;&]");
pattern!(INITIALS, r"^(?:\p{Lu}\.?\s*){1,4}$");
pattern!(ISO_DATE, r"\b(\d{4})[-/](\d{1,2})[-/](\d{1,2})\b");
pattern!(YEAR, r"\b[12]\d{3}\b");
pattern!(PAGES, r"(?i)\b([a-z]?\d+)(?:\s*[-–—−]+\s*([a-z]?\d+))?\b");

pub(crate) fn capture(regex: &Regex, text: &str) -> Option<String> {
    Some(regex.captures(text)?.get(1)?.as_str().to_owned())
}

fn trim_identifier(input: &str) -> &str {
    let mut text = input.trim_end_matches(['.', ',', ';', ':', '!', '?', '"', '\'', '”', '’']);
    while let Some(last) = text.chars().last() {
        let opening = match last {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => break,
        };
        if text.matches(last).count() <= text.matches(opening).count() {
            break;
        }
        text = text[..text.len() - last.len_utf8()].trim_end_matches(['.', ',', ';']);
    }
    text
}

fn is_valid_doi_suffix(candidate: &str) -> bool {
    let Some((_, suffix)) = candidate.split_once('/') else {
        return false;
    };
    suffix.len() >= 3 && suffix.chars().any(|c| c.is_ascii_digit())
}

fn is_external_data_doi(candidate: &str) -> bool {
    let lower = candidate.to_ascii_lowercase();
    lower.starts_with("10.5281/zenodo")
        || lower.starts_with("10.6084/m9.figshare")
        || lower.starts_with("10.17605/osf")
        || lower.starts_with("10.5061/dryad")
        || lower.starts_with("10.48420/")
}

fn is_correction_target(text: &str, match_start: usize) -> bool {
    let mut start = match_start.saturating_sub(100);
    while start < match_start && !text.is_char_boundary(start) {
        start += 1;
    }
    let before = text[start..match_start].to_ascii_lowercase();
    for needle in ["correction to:", "corrigendum to:", "erratum to:", "retraction of:"] {
        if let Some(pos) = before.rfind(needle) {
            let between = &before[pos + needle.len()..];
            if !DOI.is_match(between) {
                return true;
            }
        }
    }
    false
}

fn decoded_url(text: &str) -> String {
    let mut out = Vec::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(value) = u8::from_str_radix(hex, 16)
        {
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

fn normalize_doi(raw: &str) -> Option<String> {
    let raw = trim_identifier(raw.split(['?', '#']).next()?);
    let mut candidate = raw.replace([' ', '\t'], "");
    for suffix in ["/full", "/pdf", "/epdf", "/abstract", "/meta"] {
        if candidate.to_ascii_lowercase().ends_with(suffix) {
            candidate.truncate(candidate.len() - suffix.len());
            break;
        }
    }
    is_valid_doi_suffix(&candidate).then_some(candidate)
}

pub(crate) fn doi(text: &str) -> Option<String> {
    let decoded;
    let text = if text.contains('%') {
        decoded = decoded_url(text);
        &decoded
    } else {
        text
    };
    let wrapped = DOI_WRAPPED
        .captures_iter(text)
        .filter_map(|cap| {
            let next = trim_identifier(&cap[2]);
            // A numeric year or a standalone word starting a new sentence is not a continuation.
            if next.is_empty()
                || (next.len() == 4 && next.parse::<u16>().is_ok())
                || !next.bytes().any(|b| b.is_ascii_digit())
            {
                return None;
            }
            Some((cap.get(0)?.start(), format!("{}{next}", &cap[1])))
        })
        .collect::<Vec<_>>();
    let mut candidates = wrapped
        .iter()
        .map(|(pos, value)| (*pos, value.as_str()))
        .chain(DOI.find_iter(text).map(|m| (m.start(), m.as_str())))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(pos, _)| *pos);
    let mut fallback = None;
    for (start, raw) in candidates {
        let Some(mut candidate) = normalize_doi(raw) else {
            continue;
        };
        if let Some(asset) = SUB_ASSET_DOI.find(&candidate) {
            candidate.truncate(asset.start());
        } else if !is_external_data_doi(&candidate) && !is_correction_target(text, start) {
            return Some(candidate);
        }
        fallback.get_or_insert(candidate);
    }
    fallback
}

pub(crate) fn article_doi_from_links(links: &[nosco_pdf::PdfLink]) -> Option<String> {
    for link in links.iter().filter(|link| link.page <= 6) {
        let lower = link.uri.to_ascii_lowercase();
        if lower.contains("crossmark.crossref.org")
            && let Some(pos) = lower.find("doi=")
            && let Some(found) = doi(link.uri[pos + 4..].split('&').next()?)
        {
            return Some(found);
        }
    }
    links
        .iter()
        .filter(|link| link.page <= 6)
        .filter_map(|link| {
            let path = link.uri.split(['?', '#']).next()?;
            doi(path)
        })
        .find(|id| !is_external_data_doi(id))
}

pub(crate) fn arxiv(text: &str) -> Option<String> {
    ARXIV.captures_iter(text).find_map(|cap| {
        let id = cap[1].replace([' ', '\t'], "");
        let date = id.rsplit('/').next()?.get(..4)?;
        let month = date[2..].parse::<u8>().ok()?;
        let year = date[..2].parse::<u8>().ok()?;
        let valid_year = if id.contains('/') {
            year >= 91 || year <= 7
        } else {
            year > 7 || (year == 7 && month >= 4)
        };
        (valid_year && (1..=12).contains(&month)).then_some(id)
    })
}

pub(crate) fn arxiv_from_links(links: &[nosco_pdf::PdfLink]) -> Option<String> {
    links
        .iter()
        .filter(|link| link.page <= 6)
        .find_map(|link| arxiv(&decoded_url(&link.uri)))
}

fn digits(text: &str) -> Vec<u32> {
    text.chars()
        .filter_map(|c| {
            c.to_digit(10)
                .or_else(|| c.eq_ignore_ascii_case(&'x').then_some(10))
        })
        .collect()
}

pub(crate) fn isbn(text: &str) -> Option<String> {
    let mut best = None;
    for cap in ISBN.captures_iter(text) {
        let value = cap[2]
            .replace([' ', '\t', '-', '–', '—', '−', '‐'], "")
            .to_ascii_uppercase();
        if cap
            .get(1)
            .is_some_and(|label| label.as_str().parse::<usize>().ok() != Some(value.len()))
        {
            continue;
        }
        let digits = digits(&value);
        let valid = match digits.as_slice() {
            [9, 7, 8 | 9, ..] if digits.len() == 13 => {
                digits.iter().all(|&d| d < 10)
                    && digits
                        .iter()
                        .enumerate()
                        .map(|(i, d)| d * if i % 2 == 0 { 1 } else { 3 })
                        .sum::<u32>()
                        % 10
                        == 0
            }
            [first @ .., _] if digits.len() == 10 => {
                first.iter().all(|&d| d < 10)
                    && digits
                        .iter()
                        .enumerate()
                        .map(|(i, d)| d * (10 - i as u32))
                        .sum::<u32>()
                        % 11
                        == 0
            }
            _ => false,
        };
        if valid && digits.len() == 13 {
            return Some(value);
        }
        if valid {
            best.get_or_insert(value);
        }
    }
    best
}

pub(crate) fn issn(text: &str, electronic: bool) -> Option<String> {
    ISSN.captures_iter(text).find_map(|cap| {
        let prefix = cap.get(1).map_or("", |m| m.as_str());
        if electronic != matches!(prefix.as_bytes().first(), Some(b'e' | b'E' | b'o' | b'O')) {
            return None;
        }
        let digits = digits(&cap[2]);
        if digits
            .iter()
            .enumerate()
            .map(|(i, d)| d * (8 - i as u32))
            .sum::<u32>()
            % 11
            != 0
        {
            return None;
        }
        let value = cap[2]
            .replace([' ', '\t', '-', '–', '—', '−'], "")
            .to_ascii_uppercase();
        Some(format!("{}-{}", &value[..4], &value[4..]))
    })
}

pub(crate) fn valid_orcid(value: &str) -> bool {
    let digits = digits(value);
    let [body @ .., check] = digits.as_slice() else {
        return false;
    };
    body.len() == 15
        && (12 - body.iter().fold(0, |total, digit| (total + digit) * 2) % 11) % 11 == *check
}

pub(crate) fn emails_from_links(links: &[nosco_pdf::PdfLink]) -> Vec<String> {
    let mut result = Vec::new();
    for link in links.iter().filter(|link| link.page <= 6) {
        if let Some(rest) = link
            .uri
            .strip_prefix("mailto:")
            .or_else(|| link.uri.strip_prefix("MAILTO:"))
        {
            let addr = rest.split('?').next().unwrap_or(rest).trim();
            for email in emails(addr) {
                if !result.iter().any(|e: &String| e.eq_ignore_ascii_case(&email)) {
                    result.push(email);
                }
            }
        }
    }
    result
}

pub(crate) fn orcids_from_links(links: &[nosco_pdf::PdfLink]) -> Vec<(nosco_pdf::BoundingBox, String)> {
    let mut result = Vec::new();
    for link in links.iter().filter(|link| link.page <= 6) {
        if link.uri.to_ascii_lowercase().contains("orcid.org/")
            && let Some(id) = capture(&ORCID, &link.uri).filter(|id| valid_orcid(id))
        {
            result.push((link.bbox, id.to_ascii_uppercase()));
        }
    }
    result
}

pub(crate) fn urls(text: &str) -> impl Iterator<Item = &str> {
    URL.find_iter(text).map(|m| trim_identifier(m.as_str()))
}

pub(crate) fn emails(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    for m in EMAIL.find_iter(text) {
        let value = m.as_str().trim_start_matches('.');
        if !result.iter().any(|s: &String| s.eq_ignore_ascii_case(value)) {
            result.push(value.to_owned());
        }
    }
    for groups in GROUP_EMAIL.captures_iter(text) {
        for name in groups[1].split([',', ';']).map(str::trim) {
            let value = format!("{name}@{}", &groups[2]);
            if EMAIL.find(&value).is_some_and(|m| m.as_str() == value)
                && !result.iter().any(|s| s.eq_ignore_ascii_case(&value))
            {
                result.push(value);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_identifiers_without_syntax_punctuation() {
        let text = "(https://doi.org/10.1016/S0012-821X(03)00278-0). arXiv:hep-th/9901001 PMID: 12345 PMCID: PMC5678. https://orcid.org/0000-0002-1825-0097 alice+lab@example.edu.";
        assert_eq!(doi(text).as_deref(), Some("10.1016/S0012-821X(03)00278-0"));
        assert_eq!(capture(&ARXIV, text).as_deref(), Some("hep-th/9901001"));
        assert_eq!(capture(&PMID, text).as_deref(), Some("12345"));
        assert_eq!(capture(&PMCID, text).as_deref(), Some("PMC5678"));
        assert_eq!(capture(&ORCID, text).as_deref(), Some("0000-0002-1825-0097"));
        assert_eq!(emails(text), ["alice+lab@example.edu"]);
        assert_eq!(emails("{alice, bob}@example.org"), ["alice@example.org", "bob@example.org"]);
    }

    #[test]
    fn rejoins_line_wrapped_plos_doi() {
        let wrapped = "Citation: Smith J (2026) PLOS Biol. https://doi.org/10.1371/journal.\npbio.3004065";
        assert_eq!(doi(wrapped).as_deref(), Some("10.1371/journal.pbio.3004065"));
    }

    #[test]
    fn prefers_correction_article_doi_and_crossmark_links() {
        let corr = "Correction to: Nature Communications https://doi.org/10.1038/s41467-024-50340-x, published online 15 July 2024\nhttps://doi.org/10.1038/s41467-026-79730-z";
        assert_eq!(doi(corr).as_deref(), Some("10.1038/s41467-026-79730-z"));

        let links = vec![
            nosco_pdf::PdfLink {
                page: 1,
                bbox: nosco_pdf::BoundingBox { page: 1, x: 0.0, y: 0.0, width: 10.0, height: 10.0 },
                uri: "http://crossmark.crossref.org/dialog/?doi=10.1038/s41467-026-78753-w&domain=pdf".into(),
            },
        ];
        assert_eq!(article_doi_from_links(&links).as_deref(), Some("10.1038/s41467-026-78753-w"));
    }
}

#[test]
fn validates_book_and_serial_identifiers() {
    assert_eq!(isbn("ISBN 0-406-46554-X"), None);
    assert_eq!(
        isbn("ISBN (hbk) 0–387—32458− 5").as_deref(),
        Some("0387324585")
    );
    assert_eq!(
        isbn("ISBN-10: 0-387-32458-5\neISBN-13: 978 − 0 387 − 32458 − 6").as_deref(),
        Some("9780387324586")
    );
    assert_eq!(isbn("e-ISBN (pbk): 978-0-387-32458-9"), None);
    assert_eq!(issn("1998-2001", false), None);
    assert_eq!(issn("ISSN 1998-2001", false), None);
    assert_eq!(issn("e-ISSN 2041-1723", false), None);
    assert_eq!(issn("e-ISSN 2041–1723", true).as_deref(), Some("2041-1723"));
    assert_eq!(issn("ISSN 0378-5955", false).as_deref(), Some("0378-5955"));
    assert!(valid_orcid("0000-0002-1825-0097"));
    assert!(valid_orcid("0000-0002-1694-233X"));
    assert!(!valid_orcid("0000-0002-1825-0098"));
}

#[test]
fn handles_encoded_and_wrapped_dois_without_gluing_years() {
    assert_eq!(
        doi("https://journals.example/doi/10.1016%2fS0012-821X%2803%2900278-0/epdf?query=x#frag")
            .as_deref(),
        Some("10.1016/S0012-821X(03)00278-0")
    );
    assert_eq!(
        doi("doi:10.1016/ j.cell.2020.01.001.\n2024. A new sentence.").as_deref(),
        Some("10.1016/j.cell.2020.01.001")
    );
    assert_eq!(
        doi("doi: 10.1000/\nabc123").as_deref(),
        Some("10.1000/abc123")
    );
    assert_eq!(
        doi("10.1371/journal. 10.1371/journal.pbio.3004065").as_deref(),
        Some("10.1371/journal.pbio.3004065")
    );
    assert_eq!(doi("10.1000/"), None);
}

#[test]
fn extracts_arxiv_from_doi_html_labels_and_links() {
    assert_eq!(
        arxiv("10.48550/arXiv.2301.12345").as_deref(),
        Some("2301.12345")
    );
    assert_eq!(
        arxiv("arXiv 2301 . 12345v2").as_deref(),
        Some("2301.12345v2")
    );
    assert_eq!(arxiv("arXiv: 2313.12345"), None);
    assert_eq!(
        arxiv_from_links(&[nosco_pdf::PdfLink {
            page: 3,
            bbox: nosco_pdf::BoundingBox::new(3, 0., 0., 0., 0.),
            uri: "https://arxiv.org/html/2301.12345v2".into(),
        }])
        .as_deref(),
        Some("2301.12345v2")
    );
}

#[test]
fn isbn_does_not_interpret_library_control_numbers_as_isbns() {
    for text in [
        "2018008676",
        "LCCN: 2018008676",
        "Library of Congress Control Number: 2018008676",
        "Library of Congress Cataloging-in-Publication Data\n2018008676",
        "ISBN-13: 2018008676",
    ] {
        assert_eq!(isbn(text), None, "{text}");
    }
    assert_eq!(isbn("ISBN-10: 2018008676").as_deref(), Some("2018008676"));
    assert_eq!(
        isbn("ISBN 0-387-32458-5\nLCCN 2018008676\nISBN 978-0-387-32458-6").as_deref(),
        Some("9780387324586")
    );
}
