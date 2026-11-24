use std::ops::Range;

use nosco_crf::DecodeScratch;
use nosco_features::{
    extract_affiliation_features, extract_citation_features, extract_date_features,
    extract_name_citation_features, extract_name_header_features,
};
use nosco_lexicon::tokenizer::{tokenize, tokenize_spans, tokenize_with_spaces};
use nosco_models::{Affiliation, BibItem, Date, Person};

use crate::identifiers::*;
use crate::metadata::{
    clean_citation_title, clean_name_field, is_affiliation_symbol, recover_code_citation,
    repair_metadata_text,
};
use crate::spans::{append, clean, compact, spans};
use crate::{Engine, EngineError, ModelKind};

struct TextInput<'a> {
    source: &'a str,
    tokens: Vec<String>,
    offsets: Vec<Range<usize>>,
    line_starts: Vec<bool>,
}

impl<'a> TextInput<'a> {
    fn new(source: &'a str) -> Self {
        let tokens = tokenize(source);
        let mut last = 0;
        let mut offsets = Vec::with_capacity(tokens.len());
        let mut line_starts = Vec::with_capacity(tokens.len());
        for (start, end, _) in tokenize_spans(source) {
            line_starts.push(last == 0 || source[last..start].contains(['\n', '\r', '\x0b', '\x0c', '\u{85}']));
            offsets.push(start..end);
            last = end;
        }
        Self { source, tokens, offsets, line_starts }
    }

    fn terms(&self) -> Vec<&str> {
        self.tokens.iter().map(String::as_str).collect()
    }

    fn text(&self, rows: Range<usize>, offsets: &[Range<usize>]) -> &'a str {
        &self.source[offsets[rows.start].start..offsets[rows.end - 1].end]
    }

    fn filtered(&self, predicate: impl Fn(&str) -> bool) -> Vec<Range<usize>> {
        self.tokens.iter().zip(&self.offsets).filter(|(t, _)| predicate(t))
            .map(|(_, range)| range.clone()).collect()
    }

    // Citation features split analyzer tokens at digit/letter boundaries.
    fn citation_offsets(&self) -> Vec<Range<usize>> {
        let mut result = Vec::new();
        for (token, range) in self.tokens.iter().zip(&self.offsets) {
            let raw = &self.source[range.clone()];
            let mut start = range.start;
            let mut previous = ' ';
            for (current, (offset, _)) in token.chars().zip(raw.char_indices()) {
                if (previous.is_ascii_digit() && !current.is_ascii_digit())
                    || (nosco_features::factory::java_letter(previous) && current.is_ascii_digit())
                {
                    let end = range.start + offset;
                    if visible(&self.source[start..end]) {
                        result.push(start..end);
                    }
                    start = end;
                }
                previous = current;
            }
            if visible(&self.source[start..range.end]) {
                result.push(start..range.end);
            }
        }
        result
    }
}

fn visible(text: &str) -> bool {
    !text.trim().is_empty() && !text.contains("@IMAGE") && !text.contains("@PAGE")
}

fn date_tokens(text: &str) -> bool {
    !text.trim().is_empty() && !text.starts_with("@IMAGE")
        && ![".pbm", ".svg", ".jpg", ".png"].iter().any(|extension| text.contains(extension))
}

fn day(value: &str) -> Option<u8> {
    let value = value.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    ["st", "nd", "rd", "th"].iter().find_map(|suffix| value.strip_suffix(suffix)).unwrap_or(value)
        .parse::<u8>().ok().filter(|n| (1..=31).contains(n))
}

fn month(value: &str) -> Option<u8> {
    let text = value.trim_matches(|c: char| !c.is_alphanumeric()).to_ascii_lowercase();
    let month = match text.as_str() {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => text.parse().ok().filter(|n| (1..=12).contains(n))?,
    };
    Some(month)
}

pub(crate) fn year(value: &str) -> Option<i32> {
    let value = value.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    (value.len() == 4)
        .then(|| value.parse().ok())
        .flatten()
        .filter(|year| (1450..=2035).contains(year))
}

pub(crate) fn repair_pdf_ligature_glyph(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('Ɵ') {
        std::borrow::Cow::Owned(text.replace('Ɵ', "ti"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

pub(crate) fn joins_split_ligature(
    left: &str,
    right: &str,
    lexicon: &nosco_lexicon::Lexicon,
) -> bool {
    if !left.ends_with("fi") && !left.ends_with("fl") {
        return false;
    }
    let left_word = left
        .rsplit(|c: char| !c.is_alphabetic())
        .next()
        .unwrap_or("");
    let right_word = right
        .split(|c: char| !c.is_alphabetic())
        .next()
        .unwrap_or("");
    if left_word.eq_ignore_ascii_case("wifi") || left_word.eq_ignore_ascii_case("hifi") {
        return false;
    }
    right_word.starts_with(char::is_lowercase)
        && left_word
            .strip_suffix("fi")
            .or_else(|| left_word.strip_suffix("fl"))
            .is_some_and(|stem| !stem.is_empty())
        && (lexicon.in_dictionary(&format!("{left_word}{right_word}"))
            || (!lexicon.in_dictionary(left_word)
                && !lexicon.in_dictionary(right_word)
                && right_word.len() >= 3))
}

impl Engine {
    pub fn parse_date(&self, text: &str) -> Result<Vec<Date>, EngineError> {
        self.parse_date_with(text, &mut DecodeScratch::new())
    }

    pub(crate) fn parse_date_with(&self, text: &str, scratch: &mut DecodeScratch) -> Result<Vec<Date>, EngineError> {
        let input = TextInput::new(text);
        let features = extract_date_features(&input.terms(), self.lexicon());
        let labels = self.tag(ModelKind::Date, &features, scratch)?;
        let offsets = input.filtered(date_tokens);
        let mut dates = Vec::new();
        let mut date = Date::default();
        let mut range = 0..0;
        let mut iso_dates = ISO_DATE.captures_iter(text).peekable();
        for span in spans(&labels) {
            let piece = input.text(span.rows.clone(), &offsets);
            let start = offsets[span.rows.start].start;
            let end = offsets[span.rows.end - 1].end;
            while iso_dates.peek().is_some_and(|c| c.get(0).unwrap().end() <= start) {
                push_date(&mut dates, &mut date, text, range.clone());
                let captures = iso_dates.next().unwrap();
                dates.push(iso_date(&captures));
            }
            if iso_dates.peek().is_some_and(|c| c.get(0).unwrap().start() < end) {
                continue;
            }
            let (normalized, repeated) = match span.tag {
                "<year>" => (year(piece).is_some(), date.year.is_some()),
                "<month>" => (month(piece).is_some(), date.month.is_some()),
                "<day>" => (day(piece).is_some(), date.day.is_some()),
                _ => continue,
            };
            if !normalized {
                continue;
            }
            if range.end > range.start
                && (repeated || text[range.end..start].contains([';', '\n'])
                    || text[range.end..start].split_whitespace().any(|word| word.eq_ignore_ascii_case("and")))
            {
                push_date(&mut dates, &mut date, text, range.clone());
            }
            if date.is_empty() {
                range.start = start;
            }
            range.end = end;
            match span.tag {
                "<year>" => date.year = year(piece),
                "<month>" => date.month = month(piece),
                "<day>" => date.day = day(piece),
                _ => {}
            }
        }
        push_date(&mut dates, &mut date, text, range);
        dates.extend(iso_dates.map(|captures| iso_date(&captures)));
        Ok(dates)
    }

    pub fn parse_header_names(&self, text: &str) -> Result<Vec<Person>, EngineError> {
        self.parse_names(text, ModelKind::NameHeader, &mut DecodeScratch::new())
    }

    pub fn parse_citation_names(&self, text: &str) -> Result<Vec<Person>, EngineError> {
        self.parse_names(text, ModelKind::NameCitation, &mut DecodeScratch::new())
    }

    pub(crate) fn parse_names(&self, text: &str, kind: ModelKind, scratch: &mut DecodeScratch) -> Result<Vec<Person>, EngineError> {
        let mut people = Vec::new();
        let mut start = 0;
        for orcid in ORCID.captures_iter(text).filter(|cap| valid_orcid(&cap[1])) {
            let matched = orcid.get(0).unwrap();
            self.name_list(&text[start..matched.start()], kind, scratch, &mut people)?;
            if let Some(person) = people.last_mut() {
                person.orcid = Some(orcid[1].to_ascii_uppercase());
            }
            start = matched.end();
        }
        self.name_list(&text[start..], kind, scratch, &mut people)?;
        for person in &mut people {
            clean_name_field(&mut person.first_name);
            clean_name_field(&mut person.middle_name);
            clean_name_field(&mut person.last_name);
        }
        people.retain(has_name);
        Ok(people)
    }

    fn name_list(
        &self, text: &str, kind: ModelKind, scratch: &mut DecodeScratch, people: &mut Vec<Person>,
    ) -> Result<(), EngineError> {
        for chunk in NAME_SEPARATOR.split(text).map(clean).filter(|s| !s.is_empty()) {
            self.name_chunk(chunk, kind, scratch, people)?;
        }
        Ok(())
    }

    fn name_chunk(
        &self, chunk: &str, kind: ModelKind, scratch: &mut DecodeScratch, people: &mut Vec<Person>,
    ) -> Result<(), EngineError> {
        let normalized_chunk: String = chunk
            .chars()
            .map(|c| match c {
                '’' | '‘' | 'ʼ' => '\'',
                _ => c,
            })
            .collect();
        let chunk = normalized_chunk.as_str();
        let mut prepared = String::new();
        let mut last = 0;
        if kind == ModelKind::NameHeader {
            for marker in ATTACHED_NAME_MARKER.find_iter(chunk) {
                prepared.push_str(&chunk[last..marker.start()]);
                prepared.push(' ');
                prepared.push_str(marker.as_str());
                prepared.push(' ');
                last = marker.end();
            }
        }
        prepared.push_str(&chunk[last..]);
        let input = TextInput::new(&prepared);
        let spaced = tokenize_with_spaces(&prepared);
        let terms: Vec<_> = spaced.iter().map(String::as_str).collect();
        let features = match kind {
            ModelKind::NameHeader => extract_name_header_features(&terms, &[], self.lexicon()),
            _ => extract_name_citation_features(&terms, self.lexicon()),
        };
        let labels = self.tag(kind, &features, scratch)?;
        let offsets = input.filtered(visible);
        let mut person = Person::default();
        let mut start = 0;
        let mut end = 0;
        for span in spans(&labels) {
            let raw = input.text(span.rows.clone(), &offsets);
            let value = clean(raw);
            if value.is_empty() {
                continue;
            }
            let at = offsets[span.rows.start].start;
            let marker = span.tag == "<marker>"
                || (kind == ModelKind::NameHeader && value.chars().all(is_affiliation_symbol));
            if marker {
                person.affiliation_markers.extend(markers(value));
                person.corresp |= value.contains('*');
                continue;
            }
            let is_name = matches!(span.tag, "<title>" | "<forename>" | "<middlename>" | "<surname>" | "<suffix>");
            if !is_name {
                continue;
            }
            if is_credential_only(value) {
                if has_name(&person) {
                    append(&mut person.suffix, &compact(value));
                } else if let Some(prev) = people.last_mut() {
                    append(&mut prev.suffix, &compact(value));
                }
                continue;
            }
            let new_person = match span.tag {
                "<title>" => has_name(&person),
                "<forename>" => person.first_name.is_some()
                    || (person.last_name.is_some() && !person.affiliation_markers.is_empty()),
                "<surname>" => person.last_name.is_some(),
                _ => false,
            };
            if new_person {
                push_person(people, &mut person, &prepared[start..end]);
                start = at;
            } else if !has_name(&person) && person.title.is_none() {
                start = at;
            }
            end = offsets[span.rows.end - 1].end;
            let field = match span.tag {
                "<title>" => &mut person.title,
                "<forename>" => &mut person.first_name,
                "<middlename>" => &mut person.middle_name,
                "<surname>" => &mut person.last_name,
                _ => &mut person.suffix,
            };
            append(field, &compact(value));
        }
        push_person(people, &mut person, &prepared[start..end]);
        // Initials after an explicit comma disambiguate inverted names, even
        // when the model treats the punctuation as a new author boundary.
        if kind == ModelKind::NameCitation {
            fix_inverted(chunk, people);
        }
        Ok(())
    }

    pub fn parse_affiliations(&self, text: &str) -> Result<Vec<Affiliation>, EngineError> {
        let mut output = self.parse_affiliations_with(text, &mut DecodeScratch::new())?;
        set_affiliation_keys(&mut output);
        Ok(output)
    }

    pub(crate) fn parse_affiliations_with(&self, text: &str, scratch: &mut DecodeScratch) -> Result<Vec<Affiliation>, EngineError> {
        let input = TextInput::new(text);
        let terms = input.terms();
        let features = extract_affiliation_features(&terms, &input.line_starts, self.lexicon());
        let labels = self.tag(ModelKind::AffiliationAddress, &features, scratch)?;
        let mut output = Vec::new();
        let offsets =
            input.filtered(|token| !token.trim().is_empty() && !token.contains("@newline"));
        let mut affiliation = Affiliation::default();
        let mut raw_start = 0;
        let mut previous_end = 0;
        for span in spans(&labels) {
            let range = &offsets[span.rows.start];
            let raw = input.text(span.rows.clone(), &offsets);
            let value = clean(raw).trim_end_matches('.');
            if value.is_empty() {
                continue;
            }
            let end = offsets[span.rows.end - 1].end;
            let newline = text[previous_end..range.start].contains('\n');
            let explicit_marker = AFFILIATION_LINE_MARKER.captures(raw)
                .is_some_and(|c| clean(c.get(0).unwrap().as_str()) == clean(raw))
                || (raw.chars().all(is_affiliation_symbol) && newline);
            let new_group = match span.tag {
                "<institution>" => !affiliation.institutions.is_empty(),
                "<department>" => !affiliation.departments.is_empty(),
                "<laboratory>" => !affiliation.laboratories.is_empty(),
                _ => false,
            };
            let address_complete = affiliation.address.as_ref().is_some_and(|a| a.country.is_some());
            if (new_group && (newline || address_complete)) || ((span.tag == "<marker>" || explicit_marker)
                && (newline || affiliation.marker.is_some()))
            {
                push_affiliation(&mut output, &mut affiliation, &text[raw_start..range.start]);
                raw_start = range.start;
            }
            previous_end = end;
            if explicit_marker {
                append(&mut affiliation.marker, value);
                continue;
            }
            match span.tag {
                "<marker>" => append(&mut affiliation.marker, value),
                "<institution>" => affiliation.institutions.push(compact(value)),
                "<department>" => affiliation.departments.push(compact(value)),
                "<laboratory>" => affiliation.laboratories.push(compact(value)),
                "<other>" => {}
                tag => {
                    let address = affiliation.address.get_or_insert_with(Default::default);
                    let field = match tag {
                        "<addrLine>" => &mut address.addr_line,
                        "<postCode>" => &mut address.post_code,
                        "<postBox>" => &mut address.post_box,
                        "<settlement>" => &mut address.settlement,
                        "<region>" => &mut address.region,
                        "<country>" => &mut address.country,
                        _ => continue,
                    };
                    append(field, &compact(value));
                }
            }
        }
        push_affiliation(&mut output, &mut affiliation, &text[raw_start..]);
        if output.is_empty() && !clean(text).is_empty() {
            output.push(Affiliation { raw_affiliation: Some(compact(text)), ..Default::default() });
        }
        for aff in &mut output {
            if let Some(address) = aff.address.as_mut() {
                address.country_code = address.country.as_deref().and_then(|country| self.lexicon().country_code(country)).map(str::to_owned);
            }
        }
        Ok(output)
    }

    pub fn parse_citation(&self, text: &str) -> Result<BibItem, EngineError> {
        self.parse_citation_with(text, &mut DecodeScratch::new())
    }

    pub(crate) fn parse_citation_with(&self, text: &str, scratch: &mut DecodeScratch) -> Result<BibItem, EngineError> {
        let input = TextInput::new(text);
        let features = extract_citation_features(&input.terms(), &[], self.lexicon());
        let labels = self.tag(ModelKind::Citation, &features, scratch)?;
        let offsets = input.citation_offsets();
        let mut item = BibItem { raw_citation: compact(text), ..Default::default() };
        let mut authors = None;
        let mut editors = None;
        let mut date = None;
        let mut publication_number = None;
        let mut web = None;
        for span in spans(&labels) {
            let value = compact(clean(input.text(span.rows, &offsets)));
            let field = match span.tag {
                "<author>" => &mut authors,
                "<editor>" => &mut editors,
                "<date>" => &mut date,
                "<title>" => &mut item.title,
                "<journal>" => &mut item.journal,
                "<booktitle>" => &mut item.book_title,
                "<series>" => &mut item.series,
                "<volume>" => &mut item.volume,
                "<issue>" => &mut item.issue,
                "<pages>" => &mut item.page_range,
                "<publisher>" => &mut item.publisher,
                "<location>" => &mut item.location,
                "<institution>" => &mut item.institution,
                "<note>" | "<tech>" | "<collaboration>" => &mut item.note,
                "<pubnum>" => &mut publication_number,
                "<web>" => &mut web,
                _ => continue,
            };
            let value = if matches!(span.tag, "<author>" | "<editor>" | "<web>" | "<pubnum>") {
                value.as_str()
            } else {
                value.trim_matches([' ', '.', ',', ';', ':'])
            };
            append(field, value);
        }
        if let Some(authors) = authors
            && self.model_if_loaded(ModelKind::NameCitation).is_some()
        {
            item.authors = self.parse_names(&authors, ModelKind::NameCitation, scratch)?;
        }
        if let Some(editors) = editors
            && self.model_if_loaded(ModelKind::NameCitation).is_some()
        {
            item.editors = self.parse_names(&editors, ModelKind::NameCitation, scratch)?;
        }
        if let Some(date) = date {
            item.date = self
                .parse_date_with(&date, scratch)?
                .into_iter()
                .next()
                .or_else(|| {
                    let value = YEAR.find(&date)?.as_str();
                    Some(Date {
                        year: year(value),
                        raw: date,
                        ..Default::default()
                    })
                });
        }
        item.title = item
            .title
            .take()
            .map(|title| repair_metadata_text(title, self.lexicon()));
        clean_citation_title(&mut item);
        recover_code_citation(&mut item);
        for venue in [&mut item.journal, &mut item.book_title, &mut item.series] {
            *venue = venue
                .take()
                .map(|text| repair_metadata_text(text, self.lexicon()));
        }
        if let Some(parts) = item.page_range.as_deref().and_then(|pages| PAGES.captures(pages)) {
            item.first_page = Some(parts[1].to_owned());
            item.last_page = parts.get(2).map(|part| expand_page(&parts[1], part.as_str()));
            item.page_range = parts.get(0).map(|part| part.as_str().to_owned());
        }
        let identifier_text = publication_number.as_deref().unwrap_or_default();
        item.doi = doi(text).or_else(|| doi(identifier_text));
        item.arxiv_id = arxiv(text).or_else(|| arxiv(identifier_text));
        item.pmid = capture(&PMID, text);
        item.pmcid = capture(&PMCID, text).map(|value| value.to_ascii_uppercase());
        item.issn = issn(text, false);
        item.isbn = isbn(text);
        item.url = urls(text)
            .next()
            .or_else(|| web.as_deref().and_then(|web| urls(web).next()))
            .map(str::to_owned);
        Ok(item)
    }
}

fn expand_page(first: &str, last: &str) -> String {
    if last.len() < first.len() && last.bytes().all(|b| b.is_ascii_digit()) && first.is_ascii() {
        let candidate = format!("{}{last}", &first[..first.len()-last.len()]);
        if candidate.as_str() >= first {
            return candidate;
        }
    }
    last.to_owned()
}

fn iso_date(captures: &regex::Captures<'_>) -> Date {
    Date { year: year(&captures[1]), month: month(&captures[2]), day: day(&captures[3]), raw: captures[0].to_owned() }
}

fn push_date(dates: &mut Vec<Date>, date: &mut Date, text: &str, range: Range<usize>) {
    if !date.is_empty() {
        date.raw = compact(&text[range]);
        dates.push(std::mem::take(date));
    }
}

fn is_credential_only(value: &str) -> bool {
    let mut any = false;
    for part in value.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '/' | '(' | ')')) {
        let token: String = part
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        if token.is_empty() {
            continue;
        }
        any = true;
        let known = matches!(
            token.as_str(),
            "md" | "phd"
                | "pharmd"
                | "dphil"
                | "scd"
                | "dsc"
                | "mph"
                | "ms"
                | "msc"
                | "bs"
                | "bsc"
                | "ma"
                | "ba"
                | "mba"
                | "meng"
                | "beng"
                | "peng"
                | "pe"
                | "dds"
                | "dmd"
                | "dvm"
                | "do"
                | "rn"
                | "np"
                | "pa"
                | "mbbs"
                | "mbchb"
                | "facs"
                | "frcp"
                | "frcs"
                | "faan"
                | "fesc"
                | "facc"
                | "bst"
                | "jd"
                | "esq"
                | "mphil"
                | "mres"
                | "med"
                | "edd"
                | "psyd"
                | "drph"
                | "cpa"
        );
        if !known {
            return false;
        }
    }
    any
}

fn has_name(person: &Person) -> bool {
    person.first_name.is_some() || person.last_name.is_some()
}

fn push_person(people: &mut Vec<Person>, person: &mut Person, raw: &str) {
    if has_name(person) {
        person.raw_name = Some(compact(clean(raw)));
        people.push(std::mem::take(person));
    } else {
        *person = Person::default();
    }
}

fn fix_inverted(chunk: &str, people: &mut [Person]) {
    let Some((surname, initials)) = chunk.split_once(',') else { return };
    if !INITIALS.is_match(initials.trim()) || surname.split_whitespace().count() > 3 {
        return;
    }
    let Some(person) = people.last_mut() else { return };
    if person.raw_name.as_deref().is_none_or(|name| !chunk.contains(name)) {
        return;
    }
    let mut initials = initials.trim().chars().filter(|ch| ch.is_uppercase())
        .map(|ch| if chunk.ends_with('.') { format!("{ch}.") } else { ch.to_string() });
    person.last_name = Some(surname.trim().to_owned());
    person.first_name = initials.next();
    person.middle_name = append_initials(initials);
    person.raw_name = Some(compact(chunk));
}

fn append_initials(initials: impl Iterator<Item = String>) -> Option<String> {
    let value = initials.collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then_some(value)
}

fn push_affiliation(output: &mut Vec<Affiliation>, aff: &mut Affiliation, raw: &str) {
    if !aff.institutions.is_empty() || !aff.departments.is_empty() || !aff.laboratories.is_empty()
        || aff.address.as_ref().is_some_and(|a| !a.is_empty())
    {
        aff.raw_affiliation = Some(compact(clean(raw)));
        output.push(std::mem::take(aff));
    }
}

pub(crate) fn set_affiliation_keys(affiliations: &mut [Affiliation]) {
    for (index, aff) in affiliations.iter_mut().enumerate() {
        aff.key = Some(format!("aff{index}"));
    }
}

pub(crate) fn markers(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    for ch in text.to_lowercase().chars() {
        if let Some(number) = "⁰¹²³⁴⁵⁶⁷⁸⁹".chars().position(|sup| sup == ch) {
            current.push(char::from(b'0' + number as u8));
        } else if ch.is_alphanumeric() {
            current.push(ch);
        } else {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
            if is_affiliation_symbol(ch) {
                result.push(if ch == '∗' {
                    "*".into()
                } else {
                    ch.to_string()
                });
            }
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citation_offsets_follow_java_letter_boundaries() {
        let source = "Ⅳ12A9";
        let input = TextInput::new(source);
        let offsets = input.citation_offsets();
        let features = extract_citation_features(&input.terms(), &[], &Default::default());
        assert_eq!(features.len(), offsets.len());
        assert_eq!(
            offsets
                .iter()
                .map(|range| &source[range.clone()])
                .collect::<Vec<_>>(),
            ["Ⅳ12", "A", "9"]
        );
    }

    #[test]
    fn normalizes_month_day_and_year() {
        for (i, value) in ["Jan", "February", "MAR.", "April", "may", "Jun", "July", "Aug", "Sept", "10", "Nov", "December"].into_iter().enumerate() {
            assert_eq!(month(value), Some(i as u8 + 1));
        }
        assert_eq!(month("13"), None);
        assert_eq!(month("Januaryish"), None);
        assert_eq!(day("(31st)"), Some(31));
        assert_eq!(day("32"), None);
        assert_eq!(year("(1998)"), Some(1998));
        assert_eq!(year("98"), None);
    }
}
