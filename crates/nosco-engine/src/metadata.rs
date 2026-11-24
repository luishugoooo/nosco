use std::sync::LazyLock;

use nosco_models::{BibItem, Date, Person};
use regex::Regex;

use crate::spans::compact;
use crate::text::{joins_split_ligature, repair_pdf_ligature_glyph};

static SERIES_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:nber\s+working paper series|(?:iza\s+)?discussion paper series)\b[\s–—-]*")
        .unwrap()
});
static LEAKED_CITATION_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:(?P<names>\p{Lu}[\p{L}'’.-]{1,48},\s*(?:\p{Lu}\.\s*){1,4}",
        r"(?:(?:[&,;]|and\b)\s*\p{Lu}[\p{L}'’.-]{1,48},\s*(?:\p{Lu}\.\s*){1,4})*",
        r"|[^.]{3,80}(?:,|&|(?i:\band\b))[^.]{1,50}\.)\s+)?",
        r"(?P<raw>(?P<year>(?:19|20)\d{2})[a-zA-Z]?)\.\s+"
    ))
    .unwrap()
});

pub(crate) fn strip_series_prefix(text: &str) -> &str {
    SERIES_PREFIX.find(text).map_or(text, |prefix| {
        let rest = &text[prefix.end()..];
        if rest.starts_with(':') { text } else { rest }
    })
}

pub(crate) fn recover_code_citation(item: &mut BibItem) {
    static CODE_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([A-Z]+[0-9]+)\.\s+((?:18|19|20)[0-9]{2}[a-zA-Z]?)\.").unwrap()
    });
    let Some(prefix) = CODE_REFERENCE.captures(&item.raw_citation) else {
        return;
    };
    if item.authors.is_empty() {
        let code = prefix[1].to_owned();
        item.authors.push(Person {
            last_name: Some(code.clone()),
            raw_name: Some(code),
            ..Default::default()
        });
    }
    if item.date.as_ref().is_none_or(|date| date.year.is_none()) {
        item.date = Some(Date {
            year: prefix[2][..4].parse().ok(),
            raw: prefix[2].to_owned(),
            ..Default::default()
        });
    }
}

pub(crate) fn clean_citation_title(item: &mut BibItem) {
    let Some(title) = &mut item.title else { return };
    let Some(prefix) = LEAKED_CITATION_PREFIX.captures(title) else {
        return;
    };
    let rest = title[prefix.get(0).unwrap().end()..].trim_start();
    if rest.is_empty() || (prefix.name("names").is_some() && !rest.starts_with(char::is_alphabetic))
    {
        return;
    }
    let raw = &prefix["raw"];
    let year = prefix["year"].parse().ok();
    if item.date.as_ref().is_none_or(|date| date.year.is_none()) {
        item.date = Some(Date {
            year,
            raw: raw.to_owned(),
            ..Default::default()
        });
    } else if let Some(date) = &mut item.date
        && date.year == year
        && raw.len() > 4
        && year_suffix(&date.raw, year).is_none()
    {
        date.raw = raw.to_owned();
    }
    title.drain(..title.len() - rest.len());
}

pub(crate) fn is_affiliation_symbol(ch: char) -> bool {
    "⁰¹²³⁴⁵⁶⁷⁸⁹*∗†‡§¶#⁎".contains(ch)
}

fn name_separator(ch: char) -> bool {
    ch.is_whitespace() || ch == ',' || is_affiliation_symbol(ch)
}

pub(crate) fn year_mentions(text: &str) -> impl Iterator<Item = (i32, Option<u8>)> {
    static YEAR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b((?:18|19|20)\d{2})([a-zA-Z])?\b").unwrap());
    YEAR.captures_iter(text).filter_map(|parts| {
        let year = parts[1].parse().ok()?;
        let suffix = parts
            .get(2)
            .map(|part| part.as_str().as_bytes()[0].to_ascii_lowercase());
        Some((year, suffix))
    })
}

pub(crate) fn year_suffix(text: &str, year: Option<i32>) -> Option<u8> {
    year_mentions(text)
        .find_map(|(value, suffix)| (Some(value) == year).then_some(suffix).flatten())
}

pub(crate) fn clean_name_field(field: &mut Option<String>) {
    if let Some(value) = field {
        let mut trailing = value.trim_end_matches(name_separator);
        while let Some(stem) = trailing.strip_suffix(|ch: char| ch.is_ascii_digit()) {
            let base = stem.trim_end_matches(|ch: char| ch.is_ascii_digit());
            if base.is_empty() || base.ends_with(|ch: char| ch.is_lowercase() || name_separator(ch))
            {
                trailing = base.trim_end_matches(name_separator);
            } else {
                break;
            }
        }
        value.truncate(trailing.len());
        value.retain(|ch| !is_affiliation_symbol(ch));
        let leading = value.len() - value.trim_start().len();
        value.drain(..leading);
        if value.is_empty() {
            *field = None;
        }
    }
}

pub(crate) fn repair_metadata_text(text: String, lexicon: &nosco_lexicon::Lexicon) -> String {
    let text = if text.contains(|ch: char| {
        ch.is_control()
            || matches!(
                ch,
                '\u{00ad}' | '\u{200b}'..='\u{200f}' | '\u{2060}' | '\u{feff}'
            )
    }) {
        compact(&text.replace(
            |ch: char| {
                (ch.is_control() && !ch.is_whitespace())
                    || matches!(
                        ch,
                        '\u{00ad}' | '\u{200b}'..='\u{200f}' | '\u{2060}' | '\u{feff}'
                    )
            },
            "",
        ))
    } else {
        text
    };
    let text = match repair_pdf_ligature_glyph(&text) {
        std::borrow::Cow::Owned(repaired) => repaired,
        std::borrow::Cow::Borrowed(_) => text,
    };
    let text = if text.contains(['ﬀ', 'ﬁ', 'ﬂ', 'ﬃ', 'ﬄ']) {
        let mut repaired = String::with_capacity(text.len());
        for ch in text.chars() {
            match ch {
                'ﬀ' => repaired.push_str("ff"),
                'ﬁ' => repaired.push_str("fi"),
                'ﬂ' => repaired.push_str("fl"),
                'ﬃ' => repaired.push_str("ffi"),
                'ﬄ' => repaired.push_str("ffl"),
                _ => repaired.push(ch),
            }
        }
        repaired
    } else {
        text
    };
    if !text.contains("fi ") && !text.contains("fl ") {
        return text;
    }
    let mut words = text.split(' ');
    let mut repaired = words.next().unwrap_or_default().to_owned();
    let mut joined = String::new();
    for word in words {
        let previous = repaired.rsplit(' ').next().unwrap_or_default();
        let suffix = word
            .split(|ch: char| !ch.is_alphabetic())
            .next()
            .unwrap_or_default();
        let standalone =
            if matches!(previous, "fi" | "fl") && suffix.starts_with(char::is_lowercase) {
                joined.clear();
                joined.push_str(previous);
                joined.push_str(suffix);
                lexicon.in_dictionary(&joined)
            } else {
                false
            };
        if !standalone && !joins_split_ligature(&repaired, word, lexicon) {
            repaired.push(' ');
        }
        repaired.push_str(word);
    }
    repaired
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_leaked_citation_prefix_and_recovers_year() {
        for (input, expected) in [
            ("2020. Actual title", "Actual title"),
            ("2020a. Actual title", "Actual title"),
            ("Smith, J. & Doe, A. 2020. Actual title", "Actual title"),
            (
                "Smith, John and Jane Doe. 2020b. An actual title",
                "An actual title",
            ),
        ] {
            let mut item = BibItem {
                title: Some(input.into()),
                ..Default::default()
            };
            clean_citation_title(&mut item);
            assert_eq!(item.title.as_deref(), Some(expected));
            let date = item.date.unwrap();
            assert_eq!(date.year, Some(2020));
            assert_eq!(
                date.raw,
                input
                    .split_whitespace()
                    .find(|part| part.starts_with("2020"))
                    .unwrap()
                    .trim_end_matches('.')
            );
        }
    }

    #[test]
    fn strips_series_banners_only_from_header_titles() {
        for title in [
            "NBER WORKING PAPER SERIES A Study",
            "DISCUSSION PAPER SERIES A Study",
            "Working Paper Series: A Study",
        ] {
            let mut item = BibItem {
                title: Some(title.into()),
                ..Default::default()
            };
            clean_citation_title(&mut item);
            assert_eq!(item.title.as_deref(), Some(title));
        }
        assert_eq!(
            strip_series_prefix("NBER WORKING PAPER SERIES A Study"),
            "A Study"
        );
        assert_eq!(
            strip_series_prefix("Working Paper Series: A Study"),
            "Working Paper Series: A Study"
        );
        assert_eq!(
            strip_series_prefix("DISCUSSION PAPER SERIES: A Study"),
            "DISCUSSION PAPER SERIES: A Study"
        );
    }

    #[test]
    fn preserves_existing_dates_and_title_years() {
        let mut item = BibItem {
            title: Some("2020: The year in review".into()),
            date: Some(Date {
                year: Some(2021),
                ..Default::default()
            }),
            ..Default::default()
        };
        clean_citation_title(&mut item);
        assert_eq!(item.title.as_deref(), Some("2020: The year in review"));
        item.title = Some("2020a. The year in review".into());
        clean_citation_title(&mut item);
        assert_eq!(item.title.as_deref(), Some("The year in review"));
        assert_eq!(item.date.unwrap().year, Some(2021));
    }

    #[test]
    fn recovers_a_code_author_and_year_when_the_model_omits_them() {
        let mut item = BibItem {
            raw_citation: "REN21. 2015. Renewables Global Status Report.".into(),
            ..Default::default()
        };
        recover_code_citation(&mut item);
        assert_eq!(item.authors[0].last_name.as_deref(), Some("REN21"));
        assert_eq!(item.date.unwrap().year, Some(2015));
    }

    #[test]
    fn strips_name_markers_and_preserves_name_punctuation() {
        for (input, expected) in [
            ("J.¹*", Some("J.")),
            ("Marie‡ 12,", Some("Marie")),
            ("de la Cruz⁰†§¶#⁎", Some("de la Cruz")),
            ("O'Neill-Smith", Some("O'Neill-Smith")),
            ("¹,2", None),
            ("Smith1", Some("Smith")),
            ("Müller2", Some("Müller")),
            ("CW1", Some("CW1")),
            ("REN21", Some("REN21")),
            ("R2", Some("R2")),
            ("CW1† 2", Some("CW1")),
        ] {
            let mut field = Some(input.to_owned());
            clean_name_field(&mut field);
            assert_eq!(field.as_deref(), expected);
        }
    }

    #[test]
    fn repairs_title_and_venue_ligatures() {
        let lexicon = nosco_lexicon::Lexicon::default();
        for (input, expected) in [
            ("New ﬁndings on ﬂuids", "New findings on fluids"),
            (
                "fi muʼallifihi and fi ahdath",
                "fi muʼallifihi and fi ahdath",
            ),
            (
                "Wifi networks for hifi audio",
                "Wifi networks for hifi audio",
            ),
            (
                "ConnecƟons and tran\u{00ad}sitions",
                "Connections and transitions",
            ),
        ] {
            assert_eq!(repair_metadata_text(input.to_owned(), &lexicon), expected);
        }
    }
}
