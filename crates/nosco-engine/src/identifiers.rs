use std::sync::LazyLock;

use regex::Regex;

macro_rules! pattern {
    ($name:ident, $value:literal) => {
        pub(crate) static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($value).expect("valid scholarly identifier pattern"));
    };
}

pattern!(DOI, r"(?i)\b10\.\d{4,9}/[^\s<>\x22]+");
pattern!(DOI_WRAPPED, r"(?i)\b(10\.\d{4,9}/[^\s<>\x22]*[a-z0-9][./_-])[ \t]*\r?\n[ \t]*([a-z0-9][^\s<>\x22:]{1,64})");
pattern!(SUB_ASSET_DOI, r"(?i)\.(?:g|t|s)\d{3,}$");
pattern!(ARXIV, r"(?i)(?:\barxiv\s*:\s*|arxiv\.org/(?:abs|pdf)/)(\d{4}\.\d{4,5}(?:v\d+)?|[a-z][a-z.\-]*/\d{7}(?:v\d+)?)\b");
pattern!(PMID, r"(?i)(?:\bpmid\s*[:=]?\s*|pubmed\.ncbi\.nlm\.nih\.gov/)(\d+)\b");
pattern!(PMCID, r"(?i)\b(PMC\d+)\b");
pattern!(ISSN, r"(?i)(?:\bISSN\s*[:=]?\s*)?\b(\d{4}-\d{3}[\dX])\b");
pattern!(EISSN, r"(?i)\be\s*-?\s*issn\s*[:=]?\s*(\d{4}-\d{3}[\dX])\b");
pattern!(ISBN, r"(?i)\bISBN(?:-1[03])?\s*[:=]?\s*((?:97[89][ -]?)?(?:\d[ -]?){9}[\dX])\b");
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

pub(crate) fn doi(text: &str) -> Option<String> {
    let mut fallback = None;
    for m in DOI.find_iter(text) {
        let raw = m.as_str();
        // Rejoin line-wrapped DOIs such as "10.1371/journal.\npbio.3004065".
        let mut candidate = if raw.ends_with(['.', '/', '-', '_']) {
            if let Some(cap) = DOI_WRAPPED.captures(&text[m.start()..])
                && cap.get(0).is_some_and(|w| w.start() == 0)
            {
                let next = &cap[2];
                // Only join when the continuation looks like a DOI segment (contains a digit or dot/dash).
                if next.chars().any(|c| c.is_ascii_digit() || c == '.' || c == '-')
                    && !next.eq_ignore_ascii_case("november")
                    && !next.eq_ignore_ascii_case("december")
                    && !next.eq_ignore_ascii_case("october")
                {
                    trim_identifier(&format!("{}{next}", &cap[1])).to_owned()
                } else {
                    trim_identifier(raw).to_owned()
                }
            } else {
                trim_identifier(raw).to_owned()
            }
        } else {
            trim_identifier(raw).to_owned()
        };
        // Strip PLOS-style figure/table asset suffix only as a last resort; prefer clean article DOIs.
        if SUB_ASSET_DOI.is_match(&candidate) {
            if fallback.is_none()
                && let Some(m) = SUB_ASSET_DOI.find(&candidate)
            {
                fallback = Some(candidate[..m.start()].to_owned());
            }
            continue;
        }
        if is_valid_doi_suffix(&candidate) {
            if !is_external_data_doi(&candidate) && !is_correction_target(text, m.start()) {
                return Some(candidate);
            }
            if fallback.is_none() {
                fallback = Some(std::mem::take(&mut candidate));
            }
        } else if fallback.is_none() && !candidate.is_empty() {
            fallback = Some(candidate);
        }
    }
    fallback
}

pub(crate) fn article_doi_from_links(links: &[nosco_pdf::PdfLink]) -> Option<String> {
    let mut fallback = None;
    // CrossMark dialog links always identify the article itself, even in Author Corrections.
    for link in links.iter().filter(|link| link.page <= 2) {
        let lower = link.uri.to_ascii_lowercase();
        if lower.contains("crossmark.crossref.org")
            && let Some(pos) = lower.find("doi=")
        {
            let rest = &link.uri[pos + 4..];
            let raw = rest.split('&').next().unwrap_or(rest);
            if let Some(found) = doi(raw) {
                return Some(found);
            }
        }
    }
    for link in links.iter().filter(|link| link.page <= 2) {
        let lower = link.uri.to_ascii_lowercase();
        if !lower.contains("doi.org/") && !lower.contains("dx.doi.org/") {
            continue;
        }
        if let Some(found) = doi(&link.uri) {
            if !is_external_data_doi(&found) && !SUB_ASSET_DOI.is_match(&link.uri) {
                return Some(found);
            }
            if fallback.is_none() {
                fallback = Some(found);
            }
        }
    }
    fallback
}

pub(crate) fn emails_from_links(links: &[nosco_pdf::PdfLink]) -> Vec<String> {
    let mut result = Vec::new();
    for link in links.iter().filter(|link| link.page <= 2) {
        if let Some(rest) = link.uri.strip_prefix("mailto:").or_else(|| link.uri.strip_prefix("MAILTO:")) {
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
    for link in links.iter().filter(|link| link.page <= 2) {
        if link.uri.to_ascii_lowercase().contains("orcid.org/")
            && let Some(id) = capture(&ORCID, &link.uri)
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
