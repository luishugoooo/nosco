use std::collections::HashSet;

use crate::model::nonblank;
use crate::{Address, Affiliation, BibItem, Date, Header, Person};

use super::{TeiWriter, XmlResult};

impl TeiWriter<'_> {
    pub(super) fn header(
        &mut self,
        header: &Header,
        page_count: usize,
        attrs: &[(&str, &str)],
    ) -> XmlResult {
        self.element("teiHeader", attrs, |xml| {
            xml.element("fileDesc", &[], |xml| {
                xml.element("titleStmt", &[], |xml| {
                    let attrs = [("level", "a"), ("type", "main")];
                    if let Some(title) = nonblank(header.title.as_deref()) {
                        xml.leaf("title", Some(title), &attrs)
                    } else {
                        xml.empty("title", &attrs)
                    }
                })?;
                if page_count > 0 {
                    let count = page_count.to_string();
                    xml.element("extent", &[], |xml| {
                        xml.leaf(
                            "measure",
                            Some(&count),
                            &[("unit", "page"), ("quantity", &count)],
                        )
                    })?;
                }
                xml.element("publicationStmt", &[], |xml| {
                    if let Some(publisher) = nonblank(header.publisher.as_deref()) {
                        xml.leaf("publisher", Some(publisher), &[])?;
                    } else if nonblank(header.location.as_deref()).is_some()
                        || nonblank(header.copyright.as_deref()).is_some()
                        || header.publication_date.is_some()
                    {
                        xml.empty("publisher", &[])?;
                    } else {
                        return xml.empty("p", &[]);
                    }
                    xml.leaf("pubPlace", header.location.as_deref(), &[])?;
                    if let Some(copyright) = nonblank(header.copyright.as_deref()) {
                        xml.element("availability", &[], |xml| {
                            xml.leaf("p", Some(copyright), &[])
                        })?;
                    }
                    xml.date(header.publication_date.as_ref(), "published")
                })?;
                if nonblank(header.funding_note.as_deref()).is_some()
                    || nonblank(header.note.as_deref()).is_some()
                {
                    xml.element("notesStmt", &[], |xml| {
                        xml.leaf(
                            "note",
                            header.funding_note.as_deref(),
                            &[("type", "funding")],
                        )?;
                        xml.leaf("note", header.note.as_deref(), &[])
                    })?;
                }
                xml.element("sourceDesc", &[], |xml| xml.header_source(header))
            })?;
            xml.profile(header)
        })
    }

    fn header_source(&mut self, header: &Header) -> XmlResult {
        self.element("biblStruct", &[], |xml| {
            if nonblank(header.title.as_deref()).is_some()
                || header.authors.iter().any(|author| !author.is_empty())
                || header
                    .affiliations
                    .iter()
                    .any(|affiliation| !affiliation.is_empty())
            {
                xml.element("analytic", &[], |xml| {
                    xml.leaf(
                        "title",
                        header.title.as_deref(),
                        &[("level", "a"), ("type", "main")],
                    )?;
                    xml.header_authors(header)
                })?;
            }
            xml.element("monogr", &[], |xml| {
                xml.leaf("title", header.journal.as_deref(), &[("level", "j")])?;
                xml.leaf("idno", header.issn.as_deref(), &[("type", "ISSN")])?;
                xml.leaf("idno", header.eissn.as_deref(), &[("type", "eISSN")])?;
                xml.leaf("idno", header.isbn.as_deref(), &[("type", "ISBN")])?;
                for editor in &header.editors {
                    xml.person("editor", editor, &[], &mut [])?;
                }
                xml.leaf("meeting", header.meeting.as_deref(), &[])?;
                xml.element("imprint", &[], |xml| {
                    xml.leaf("publisher", header.publisher.as_deref(), &[])?;
                    xml.leaf("pubPlace", header.location.as_deref(), &[])?;
                    xml.scope("volume", header.volume.as_deref())?;
                    xml.scope("issue", header.issue.as_deref())?;
                    xml.scope("page", header.pages.as_deref())?;
                    xml.date(header.publication_date.as_ref(), "published")
                })
            })?;
            xml.idnos(&[
                ("DOI", &header.doi),
                ("arXiv", &header.arxiv_id),
                ("PMID", &header.pmid),
                ("PMCID", &header.pmcid),
            ])?;
            for url in &header.urls {
                if let Some(url) = nonblank(Some(url)) {
                    xml.empty("ptr", &[("target", url)])?;
                }
            }
            Ok(())
        })
    }

    fn header_authors(&mut self, header: &Header) -> XmlResult {
        let mut linked = vec![false; header.affiliations.len()];
        for author in &header.authors {
            self.person("author", author, &header.affiliations, &mut linked)?;
        }
        for (affiliation, linked) in header.affiliations.iter().zip(linked) {
            if !linked && !affiliation.is_empty() {
                self.element("author", &[], |xml| xml.affiliation(affiliation))?;
            }
        }
        Ok(())
    }

    fn profile(&mut self, header: &Header) -> XmlResult {
        self.element("profileDesc", &[], |xml| {
            if header.dates.iter().any(|date| !date.is_empty())
                || nonblank(header.submission_note.as_deref()).is_some()
            {
                xml.element("creation", &[], |xml| {
                    for date in &header.dates {
                        xml.date(Some(date), "")?;
                    }
                    xml.leaf(
                        "date",
                        header.submission_note.as_deref(),
                        &[("type", "submission")],
                    )
                })?;
            }
            if let Some(language) = nonblank(header.language.as_deref()) {
                xml.element("langUsage", &[], |xml| {
                    xml.empty("language", &[("ident", language)])
                })?;
            }
            if header
                .keywords
                .iter()
                .any(|keyword| !keyword.trim().is_empty())
            {
                xml.element("textClass", &[], |xml| {
                    xml.element("keywords", &[], |xml| {
                        for keyword in &header.keywords {
                            xml.leaf("term", Some(keyword), &[])?;
                        }
                        Ok(())
                    })
                })?;
            }
            if let Some(abstract_text) = nonblank(header.abstract_text.as_deref()) {
                xml.element("abstract", &[], |xml| {
                    xml.leaf("p", Some(abstract_text), &[])
                })?;
            }
            Ok(())
        })
    }

    pub(super) fn references(&mut self, refs: &[BibItem], attrs: &[(&str, &str)]) -> XmlResult {
        if refs.is_empty() {
            return self.empty("listBibl", attrs);
        }
        let explicit_ids: HashSet<_> = refs
            .iter()
            .filter_map(|item| nonblank(item.id.as_deref()))
            .collect();
        self.element("listBibl", attrs, |xml| {
            for (index, item) in refs.iter().enumerate() {
                let mut fallback = String::new();
                if nonblank(item.id.as_deref()).is_none() {
                    fallback = format!("b{index}");
                    while explicit_ids.contains(fallback.as_str()) {
                        fallback.push('_');
                    }
                }
                xml.citation(item, &fallback, &[])?;
            }
            Ok(())
        })
    }

    pub(super) fn citation(
        &mut self,
        item: &BibItem,
        fallback_id: &str,
        attrs: &[(&str, &str)],
    ) -> XmlResult {
        let coords = self.coords(&item.bboxes);
        let id = nonblank(item.id.as_deref()).unwrap_or(fallback_id);
        let label = item.label.as_deref().unwrap_or_default();
        let mut attrs = attrs.to_vec();
        attrs.extend([("xml:id", id), ("n", label), ("coords", &coords)]);
        self.element("biblStruct", &attrs, |xml| {
            let journal = nonblank(item.journal.as_deref());
            let book = nonblank(item.book_title.as_deref());
            let has_container = journal.is_some() || book.is_some();
            let has_article = nonblank(item.title.as_deref()).is_some()
                || item.authors.iter().any(|author| !author.is_empty());
            if has_container && has_article {
                xml.element("analytic", &[], |xml| {
                    xml.leaf(
                        "title",
                        item.title.as_deref(),
                        &[("level", "a"), ("type", "main")],
                    )?;
                    xml.people("author", &item.authors)
                })?;
            }
            xml.element("monogr", &[], |xml| {
                xml.leaf("title", journal, &[("level", "j")])?;
                xml.leaf("title", book, &[("level", "m")])?;
                if !has_container {
                    xml.leaf(
                        "title",
                        item.title.as_deref(),
                        &[("level", "m"), ("type", "main")],
                    )?;
                    xml.people("author", &item.authors)?;
                }
                xml.people("editor", &item.editors)?;
                if let Some(institution) = nonblank(item.institution.as_deref()) {
                    xml.element("respStmt", &[], |xml| {
                        xml.leaf("resp", Some("institution"), &[])?;
                        xml.leaf("orgName", Some(institution), &[])
                    })?;
                }
                xml.idnos(&[("ISSN", &item.issn), ("ISBN", &item.isbn)])?;
                xml.element("imprint", &[], |xml| {
                    xml.leaf("publisher", item.publisher.as_deref(), &[])?;
                    xml.leaf("pubPlace", item.location.as_deref(), &[])?;
                    xml.scope("volume", item.volume.as_deref())?;
                    xml.scope("issue", item.issue.as_deref())?;
                    xml.pages(item)?;
                    xml.date(item.date.as_ref(), "published")
                })
            })?;
            if let Some(series) = nonblank(item.series.as_deref()) {
                xml.element("series", &[], |xml| {
                    xml.leaf("title", Some(series), &[("level", "s")])
                })?;
            }
            xml.idnos(&[
                ("DOI", &item.doi),
                ("arXiv", &item.arxiv_id),
                ("PMID", &item.pmid),
                ("PMCID", &item.pmcid),
            ])?;
            if let Some(url) = nonblank(item.url.as_deref()) {
                xml.empty("ptr", &[("target", url)])?;
            }
            xml.leaf("note", item.note.as_deref(), &[])?;
            if xml.opts.include_raw_citations {
                xml.leaf(
                    "note",
                    Some(&item.raw_citation),
                    &[("type", "raw_reference")],
                )?;
            }
            Ok(())
        })
    }

    fn idnos(&mut self, identifiers: &[(&str, &Option<String>)]) -> XmlResult {
        for (kind, value) in identifiers {
            self.leaf("idno", value.as_deref(), &[("type", kind)])?;
        }
        Ok(())
    }

    fn scope(&mut self, unit: &str, value: Option<&str>) -> XmlResult {
        self.leaf("biblScope", value, &[("unit", unit)])
    }

    fn pages(&mut self, item: &BibItem) -> XmlResult {
        let first = nonblank(item.first_page.as_deref());
        let last = nonblank(item.last_page.as_deref());
        let range = nonblank(item.page_range.as_deref());
        if first.is_none() && last.is_none() {
            return self.scope("page", range);
        }
        let attrs = [
            ("unit", "page"),
            ("from", first.unwrap_or_default()),
            ("to", last.unwrap_or_default()),
        ];
        if range.is_some() {
            self.leaf("biblScope", range, &attrs)
        } else {
            self.empty("biblScope", &attrs)
        }
    }

    fn date(&mut self, date: Option<&Date>, kind: &str) -> XmlResult {
        if let Some(date) = date {
            let when = date.to_iso8601().unwrap_or_default();
            let attrs = [("type", kind), ("when", when.as_str())];
            if let Some(raw) = nonblank(Some(&date.raw)) {
                self.leaf("date", Some(raw), &attrs)?;
            } else if !when.is_empty() {
                self.empty("date", &attrs)?;
            }
        }
        Ok(())
    }

    fn people(&mut self, role: &str, people: &[Person]) -> XmlResult {
        for person in people {
            self.person(role, person, &[], &mut [])?;
        }
        Ok(())
    }

    fn person(
        &mut self,
        role: &str,
        person: &Person,
        affiliations: &[Affiliation],
        linked: &mut [bool],
    ) -> XmlResult {
        if person.is_empty() {
            return Ok(());
        }
        let corresp = if person.corresp { "corresp" } else { "" };
        self.element(role, &[("role", corresp)], |xml| {
            if let Some(organization) = person.organization.as_deref() {
                xml.leaf("orgName", Some(organization), &[])?;
            } else {
                xml.person_name(person)?;
            }
            xml.leaf("email", person.email.as_deref(), &[])?;
            xml.leaf("idno", person.orcid.as_deref(), &[("type", "ORCID")])?;
            for (affiliation, linked) in affiliations.iter().zip(linked) {
                if is_linked(person, affiliation) {
                    xml.affiliation(affiliation)?;
                    *linked = true;
                }
            }
            for key in &person.affiliation_keys {
                if nonblank(Some(key)).is_some()
                    && !affiliations.iter().any(|aff| aff.key.as_ref() == Some(key))
                {
                    xml.empty("affiliation", &[("key", key)])?;
                }
            }
            for marker in &person.affiliation_markers {
                if nonblank(Some(marker)).is_some()
                    && !affiliations
                        .iter()
                        .any(|aff| aff.marker.as_ref() == Some(marker))
                {
                    xml.element("affiliation", &[], |xml| {
                        xml.raw_affiliation(Some(marker), None)
                    })?;
                }
            }
            Ok(())
        })
    }

    fn person_name(&mut self, person: &Person) -> XmlResult {
        let parts = person.name_parts().map(nonblank);
        if parts.iter().all(Option::is_none) {
            return self.leaf("persName", person.raw_name.as_deref(), &[]);
        }
        let [title, first, middle, last, suffix] = parts;
        self.element("persName", &[], |xml| {
            xml.leaf("roleName", title, &[])?;
            xml.leaf("forename", first, &[("type", "first")])?;
            xml.leaf("forename", middle, &[("type", "middle")])?;
            xml.leaf("surname", last, &[])?;
            xml.leaf("genName", suffix, &[])
        })
    }

    fn affiliation(&mut self, affiliation: &Affiliation) -> XmlResult {
        let key = affiliation.key.as_deref().unwrap_or_default();
        self.element("affiliation", &[("key", key)], |xml| {
            xml.raw_affiliation(
                affiliation.marker.as_deref(),
                affiliation.raw_affiliation.as_deref(),
            )?;
            for (kind, names) in [
                ("department", &affiliation.departments),
                ("laboratory", &affiliation.laboratories),
                ("institution", &affiliation.institutions),
            ] {
                for name in names {
                    xml.leaf("orgName", Some(name), &[("type", kind)])?;
                }
            }
            if let Some(address) = affiliation
                .address
                .as_ref()
                .filter(|address| !address.is_empty())
            {
                xml.address(address)?;
            }
            Ok(())
        })
    }

    fn raw_affiliation(&mut self, marker: Option<&str>, raw: Option<&str>) -> XmlResult {
        let marker = nonblank(marker);
        let raw = nonblank(raw);
        if marker.is_none() && raw.is_none() {
            return Ok(());
        }
        self.element("note", &[("type", "raw_affiliation")], |xml| {
            xml.text("")?;
            xml.leaf("label", marker, &[])?;
            if marker.is_some() && raw.is_some() {
                xml.text(" ")?;
            }
            xml.text(raw.unwrap_or_default())
        })
    }

    fn address(&mut self, address: &Address) -> XmlResult {
        self.element("address", &[], |xml| {
            xml.leaf("addrLine", address.addr_line.as_deref(), &[])?;
            xml.leaf("postBox", address.post_box.as_deref(), &[])?;
            xml.leaf("postCode", address.post_code.as_deref(), &[])?;
            xml.leaf("settlement", address.settlement.as_deref(), &[])?;
            xml.leaf("region", address.region.as_deref(), &[])?;
            let code = address.country_code.as_deref().unwrap_or_default();
            if let Some(country) = nonblank(address.country.as_deref()) {
                xml.leaf("country", Some(country), &[("key", code)])?;
            } else if !code.is_empty() {
                xml.empty("country", &[("key", code)])?;
            }
            Ok(())
        })
    }
}

fn is_linked(person: &Person, affiliation: &Affiliation) -> bool {
    affiliation
        .key
        .as_ref()
        .is_some_and(|key| !key.trim().is_empty() && person.affiliation_keys.contains(key))
        || affiliation.marker.as_ref().is_some_and(|marker| {
            !marker.trim().is_empty() && person.affiliation_markers.contains(marker)
        })
}
