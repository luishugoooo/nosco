use quick_xml::Reader;
use quick_xml::events::Event;

use super::*;

const TEI_NAMESPACE: &str = "http://www.tei-c.org/ns/1.0";

fn check_xml(xml: &str, root: &str) {
    let mut reader = Reader::from_str(xml);
    let mut depth = 0;
    let mut roots = 0;
    loop {
        let event = reader.read_event().unwrap();
        match event {
            Event::Start(ref tag) | Event::Empty(ref tag) => {
                for attr in tag.attributes() {
                    attr.unwrap()
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .unwrap();
                }
                if depth == 0 {
                    roots += 1;
                    assert_eq!(tag.name().as_ref(), root);
                    let namespace = tag.try_get_attribute("xmlns").unwrap().unwrap();
                    assert_eq!(namespace.value.as_ref(), TEI_NAMESPACE);
                }
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                }
            }
            Event::End(_) => depth -= 1,
            Event::Text(text) if depth == 0 => assert!(text.xml10_content().trim().is_empty()),
            Event::GeneralRef(reference) => {
                assert!(depth > 0);
                assert!(
                    reference.resolve_char_ref().unwrap().is_some()
                        || matches!(reference.as_ref(), "lt" | "gt" | "amp" | "apos" | "quot")
                );
            }
            Event::Eof => break,
            _ => {}
        }
    }
    assert_eq!((roots, depth), (1, 0));
}

fn date(year: i32, month: Option<u8>, day: Option<u8>) -> Date {
    Date {
        year: Some(year),
        month,
        day,
        ..Date::default()
    }
}

#[test]
fn formats_partial_and_complete_iso_dates() {
    for (date, expected) in [
        (date(7, None, None), "0007"),
        (date(2024, Some(2), None), "2024-02"),
        (date(2024, Some(2), Some(9)), "2024-02-09"),
        (date(2000, Some(2), Some(29)), "2000-02-29"),
        (date(1900, Some(2), Some(29)), "1900-02"),
        (date(2024, Some(4), Some(31)), "2024-04"),
        (date(2024, Some(0), Some(1)), "2024"),
        (date(2024, Some(13), None), "2024"),
        (date(2024, None, Some(12)), "2024"),
    ] {
        assert_eq!(date.to_iso8601().as_deref(), Some(expected));
    }
    assert!(date(10_000, None, None).to_iso8601().is_none());
    assert!(Date::default().is_empty());
    let raw = Date {
        raw: "Early spring".into(),
        ..Date::default()
    };
    assert!(!raw.is_empty());
    assert!(raw.to_iso8601().is_none());
}

#[test]
fn assembles_names_and_distinguishes_empty_metadata() {
    let person = Person {
        title: Some(" Dr. ".into()),
        first_name: Some("Ana".into()),
        middle_name: Some("  ".into()),
        last_name: Some("García".into()),
        suffix: Some("Jr.".into()),
        raw_name: Some("Alternate spelling".into()),
        ..Person::default()
    };
    assert_eq!(person.full_name(), "Dr. Ana García Jr.");
    assert!(!person.is_empty());
    let person = Person {
        raw_name: Some("  Ana García  ".into()),
        ..Person::default()
    };
    assert_eq!(person.full_name(), "Ana García");
    assert!(Person::default().is_empty());
    assert!(
        Address {
            region: Some("  ".into()),
            ..Address::default()
        }
        .is_empty()
    );
    assert!(
        !Address {
            country_code: Some("DE".into()),
            ..Address::default()
        }
        .is_empty()
    );
    assert!(
        Affiliation {
            address: Some(Address::default()),
            ..Affiliation::default()
        }
        .is_empty()
    );
    assert!(
        !Affiliation {
            institutions: vec!["Institute".into()],
            ..Affiliation::default()
        }
        .is_empty()
    );
}

#[test]
fn round_trips_json_and_omits_empty_fields() {
    let doc = example_document();
    let compact = doc.to_json().unwrap();
    let pretty = doc.to_json_pretty().unwrap();
    assert!(pretty.contains("\n  \"header\""));
    for json in [compact, pretty] {
        assert_eq!(serde_json::from_str::<Document>(&json).unwrap(), doc);
        assert!(!json.contains("\"raw_name\":null"));
        assert!(!json.contains("\"affiliation_markers\""));
        assert!(!json.contains("\"subsections\":[]"));
    }
    let header = &doc.header;
    assert_eq!(
        serde_json::from_str::<Header>(&header.to_json().unwrap()).unwrap(),
        *header
    );
    assert_eq!(
        serde_json::from_str::<Header>(&header.to_json_pretty().unwrap()).unwrap(),
        *header
    );
    let reference = &doc.references[0];
    assert_eq!(
        serde_json::from_str::<BibItem>(&reference.to_json().unwrap()).unwrap(),
        *reference
    );
    assert_eq!(
        serde_json::from_str::<BibItem>(&reference.to_json_pretty().unwrap()).unwrap(),
        *reference
    );
    assert_eq!(
        serde_json::from_str::<Document>("{}").unwrap(),
        Document::default()
    );
    assert_eq!(Header::default().to_json().unwrap(), "{}");
    assert_eq!(BibItem::default().to_json().unwrap(), "{}");
}

fn callout(
    kind: CalloutKind,
    text: &str,
    target: Option<&str>,
    start: usize,
    end: usize,
) -> Callout {
    Callout {
        kind,
        text: text.into(),
        target_id: target.map(str::to_owned),
        char_start: start,
        char_end: end,
    }
}

fn paragraph_xml(paragraph: &Paragraph, pretty: bool) -> String {
    let doc = Document {
        body: vec![Section {
            elements: vec![BodyElement::Paragraph(paragraph.clone())],
            ..Section::default()
        }],
        ..Document::default()
    };
    let opts = TeiOptions {
        pretty,
        ..TeiOptions::default()
    };
    let xml = to_tei_xml(&doc, &opts).unwrap();
    check_xml(&xml, "TEI");
    xml
}

#[test]
fn injects_inline_callouts_with_unicode_byte_and_character_offsets() {
    let paragraph = Paragraph {
        text: "é[α]&文²🙂".into(),
        callouts: vec![
            callout(CalloutKind::Footnote, "²", Some("foot_1"), 6, 7),
            callout(CalloutKind::Bibr, "[α]", Some("#b0"), 2, 6),
            callout(CalloutKind::Figure, "文", Some("fig_1"), 5, 6),
            callout(CalloutKind::Formula, "🙂", None, 12, 16),
        ],
        ..Paragraph::default()
    };
    for pretty in [false, true] {
        let result = paragraph_xml(&paragraph, pretty);
        assert!(result.contains(
            "<p>é<ref type=\"bibr\" target=\"#b0\">[α]</ref>&amp;<ref type=\"figure\" target=\"#fig_1\">文</ref><ref type=\"foot\" target=\"#foot_1\">²</ref><ref type=\"formula\">🙂</ref></p>"
        ), "{result}");
    }
}

#[test]
fn skips_overlapping_and_invalid_callouts_and_keeps_adjacent_refs_inline() {
    let paragraph = Paragraph {
        text: "[1][2]é".into(),
        callouts: vec![
            callout(CalloutKind::Figure, "[1", Some("short"), 0, 2),
            callout(CalloutKind::Bibr, "[1]", Some("first"), 0, 3),
            callout(CalloutKind::Figure, "1][2", Some("overlap"), 1, 5),
            callout(CalloutKind::Table, "[2]", Some("second"), 3, 6),
            callout(CalloutKind::Footnote, "", Some("empty"), 6, 6),
            callout(CalloutKind::Bibr, "", Some("inside_codepoint"), 7, 8),
            callout(CalloutKind::Bibr, "", Some("past_end"), 6, 9),
            callout(CalloutKind::Bibr, "", Some("backwards"), 8, 7),
            callout(CalloutKind::Bibr, "", Some("overflow"), 8, usize::MAX),
            callout(CalloutKind::Bibr, "[2]", Some("duplicate"), 3, 6),
        ],
        ..Paragraph::default()
    };
    for pretty in [false, true] {
        let result = paragraph_xml(&paragraph, pretty);
        assert!(result.contains(
            "<p><ref type=\"bibr\" target=\"#first\">[1]</ref><ref type=\"table\" target=\"#second\">[2]</ref>é</p>"
        ), "{result}");
    }
}

#[test]
fn full_document_tei_covers_header_body_and_back() {
    let doc = example_document();
    let opts = TeiOptions {
        include_coordinates: true,
        include_raw_citations: true,
        ..TeiOptions::default()
    };
    let xml = doc.to_tei_xml(&opts).unwrap();
    check_xml(&xml, "TEI");
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?><TEI "));
    for fragment in [
        "<teiHeader><fileDesc><titleStmt><title level=\"a\" type=\"main\">A &amp; B &lt; C</title>",
        "<measure unit=\"page\" quantity=\"3\">3</measure>",
        "<publicationStmt><publisher>Press &amp; Co.</publisher>",
        "<availability><p>© Ana &amp; Co.</p></availability>",
        "<sourceDesc><biblStruct><analytic>",
        "<author role=\"corresp\"><persName><forename type=\"first\">Ana</forename>",
        "<affiliation key=\"aff0\"><note type=\"raw_affiliation\"><label>1</label> Dept. &amp; Lab</note>",
        "<author><affiliation key=\"aff1\">",
        "<orgName type=\"institution\">Institute &amp; Lab</orgName>",
        "<country key=\"DE\">Germany</country>",
        "<profileDesc><creation><date when=\"2023-12\"/>",
        "<date type=\"submission\">Submitted in winter</date>",
        "<langUsage><language ident=\"de\"/></langUsage>",
        "<textClass><keywords><term>Extraction &amp; OCR</term></keywords></textClass>",
        "<abstract><p>Café &lt;markup&gt;</p></abstract>",
        "<text><body><div><head n=\"1\">Introduction</head>",
        "<p coords=\"1,10.13,20.00,30.00,40.00;2,10.13,20.00,30.00,40.00\">See <ref type=\"bibr\" target=\"#b0\">[1]</ref>.</p>",
        "<formula xml:id=\"formula_0\" coords=\"1,10.13,20.00,30.00,40.00\">x &lt; y<label>(1)</label></formula>",
        "<list><item n=\"a\">An item</item></list><note>Body note</note>",
        "<div><head>Subsection</head></div>",
        "<figure xml:id=\"fig_0\" coords=\"1,10.13,20.00,30.00,40.00\"><head>Figure 1</head>",
        "<figDesc>Figure caption</figDesc>",
        "<figure type=\"table\" xml:id=\"tab_0\"",
        "<table><row><cell cols=\"2\" rows=\"3\" role=\"label\">A &amp; B</cell><cell>C</cell></row></table>",
        "<note place=\"foot\" xml:id=\"foot_1\" n=\"1\"><pb n=\"2\"/>Footnote</note>",
        "<back><div type=\"acknowledgement\"><p>Thank you.</p></div>",
        "<div type=\"annex\"><head>Supplement</head></div>",
        "<div type=\"references\"><listBibl><biblStruct xml:id=\"b0\" n=\"1\"",
        "<biblScope unit=\"page\" from=\"10\" to=\"15\">10–15</biblScope>",
        "<date type=\"published\" when=\"2024-02-29\">February 29, 2024</date>",
        "<idno type=\"DOI\">10.1000/a&amp;b</idno>",
        "<note type=\"raw_reference\">Raw &lt;reference&gt;</note>",
    ] {
        assert!(xml.contains(fragment), "Missing {fragment:?} in {xml}");
    }

    let pretty = doc
        .to_tei_xml(&TeiOptions {
            pretty: true,
            ..TeiOptions::default()
        })
        .unwrap();
    check_xml(&pretty, "TEI");
    assert!(pretty.contains("\n  <teiHeader>\n    <fileDesc>"));
    assert!(!pretty.contains("coords="));
    assert!(!pretty.contains("raw_reference"));
}

#[test]
fn standalone_header_reference_list_and_citation_are_namespaced() {
    let doc = example_document();
    let opts = TeiOptions::default();
    let header = doc.header.to_tei_xml(&opts).unwrap();
    check_xml(&header, "teiHeader");
    assert_eq!(header, header_to_tei_xml(&doc.header, &opts).unwrap());

    let book = BibItem {
        id: Some("b1".into()),
        title: Some("A Book".into()),
        ..BibItem::default()
    };
    let list = references_to_tei_xml(&[book.clone(), doc.references[0].clone()], &opts).unwrap();
    check_xml(&list, "listBibl");
    assert!(list.contains("xml:id=\"b1\""));
    assert!(list.contains("xml:id=\"b1_\""));

    let empty = references_to_tei_xml(&[], &opts).unwrap();
    check_xml(&empty, "listBibl");
    let xml = book.to_tei_xml(&opts).unwrap();
    check_xml(&xml, "biblStruct");
    assert!(xml.contains("<monogr><title level=\"m\" type=\"main\">A Book</title>"));
    assert!(!xml.contains("<analytic>"));
    assert_eq!(xml, citation_to_tei_xml(&book, &opts).unwrap());
}

#[test]
fn escapes_text_and_attributes_and_rejects_invalid_xml_characters() {
    let value = "<test a=\"&'\">";
    let url = format!("https://example.org/?q={value}");
    let item = BibItem {
        title: Some(value.into()),
        url: Some(url.clone()),
        ..BibItem::default()
    };
    let xml = item.to_tei_xml(&TeiOptions::default()).unwrap();
    check_xml(&xml, "biblStruct");
    assert!(xml.contains("&lt;test a=&quot;&amp;&apos;&quot;&gt;"));
    let mut reader = Reader::from_str(&xml);
    loop {
        match reader.read_event().unwrap() {
            Event::Empty(tag) if tag.name().as_ref() == "ptr" => {
                let attr = tag.try_get_attribute("target").unwrap().unwrap();
                assert_eq!(
                    attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .unwrap(),
                    url
                );
                break;
            }
            Event::Eof => panic!("Missing ptr element"),
            _ => {}
        }
    }

    for item in [
        BibItem {
            title: Some("bad\u{0}".into()),
            ..BibItem::default()
        },
        BibItem {
            url: Some("bad\u{0}".into()),
            ..BibItem::default()
        },
    ] {
        assert!(matches!(
            item.to_tei_xml(&TeiOptions::default()),
            Err(ModelError::InvalidXmlCharacter(0))
        ));
    }
}

fn example_document() -> Document {
    let bbox = BBox {
        page: 1,
        x: 10.126,
        y: 20.0,
        width: 30.0,
        height: 40.0,
    };
    Document {
        header: Header {
            title: Some("A & B < C".into()),
            authors: vec![Person {
                first_name: Some("Ana".into()),
                last_name: Some("García".into()),
                email: Some("ana@example.org".into()),
                orcid: Some("0000-0002-1825-0097".into()),
                affiliation_keys: vec!["aff0".into()],
                corresp: true,
                ..Person::default()
            }],
            affiliations: vec![
                Affiliation {
                    key: Some("aff0".into()),
                    marker: Some("1".into()),
                    institutions: vec!["Institute & Lab".into()],
                    departments: vec!["Department".into()],
                    raw_affiliation: Some("Dept. & Lab".into()),
                    address: Some(Address {
                        settlement: Some("Berlin".into()),
                        country: Some("Germany".into()),
                        country_code: Some("DE".into()),
                        ..Address::default()
                    }),
                    ..Affiliation::default()
                },
                Affiliation {
                    key: Some("aff1".into()),
                    laboratories: vec!["Shared Lab".into()],
                    ..Affiliation::default()
                },
            ],
            abstract_text: Some("Café <markup>".into()),
            keywords: vec!["Extraction & OCR".into()],
            dates: vec![date(2023, Some(12), None)],
            publication_date: Some(date(2024, Some(3), None)),
            journal: Some("Journal of Example Studies".into()),
            volume: Some("9".into()),
            issue: Some("2".into()),
            pages: Some("10–15".into()),
            publisher: Some("Press & Co.".into()),
            location: Some("Berlin".into()),
            doi: Some("10.1000/article".into()),
            copyright: Some("© Ana & Co.".into()),
            funding_note: Some("Research grant".into()),
            language: Some("de".into()),
            submission_note: Some("Submitted in winter".into()),
            urls: vec!["https://example.org/article?id=1&v=2".into()],
            ..Header::default()
        },
        body: vec![Section {
            number: Some("1".into()),
            title: Some("Introduction".into()),
            elements: vec![
                BodyElement::Paragraph(Paragraph {
                    text: "See [1].".into(),
                    callouts: vec![callout(CalloutKind::Bibr, "[1]", Some("b0"), 4, 7)],
                    bboxes: vec![
                        bbox.clone(),
                        BBox {
                            page: 2,
                            ..bbox.clone()
                        },
                    ],
                }),
                BodyElement::Formula(Formula {
                    id: Some("formula_0".into()),
                    label: Some("(1)".into()),
                    text: "x < y".into(),
                    bboxes: vec![bbox.clone()],
                }),
                BodyElement::List(vec![ListItem {
                    label: Some("a".into()),
                    text: "An item".into(),
                }]),
                BodyElement::Note("Body note".into()),
            ],
            subsections: vec![Section {
                title: Some("Subsection".into()),
                ..Section::default()
            }],
        }],
        figures: vec![Figure {
            id: Some("fig_0".into()),
            header: Some("Figure 1".into()),
            label: Some("1".into()),
            caption: Some("Figure caption".into()),
            content: Some("Figure text".into()),
            bboxes: vec![bbox.clone()],
        }],
        tables: vec![Table {
            id: Some("tab_0".into()),
            header: Some("Table 1".into()),
            label: Some("1".into()),
            caption: Some("Table caption".into()),
            content: Some("A & B / C".into()),
            rows: vec![TableRow {
                cells: vec![
                    TableCell {
                        text: "A & B".into(),
                        cols: 2,
                        rows: 3,
                        is_header: true,
                    },
                    TableCell {
                        text: "C".into(),
                        ..TableCell::default()
                    },
                ],
            }],
            note: Some("Table note".into()),
            bboxes: vec![bbox.clone()],
        }],
        references: vec![BibItem {
            label: Some("1".into()),
            title: Some("A prior study".into()),
            authors: vec![Person {
                last_name: Some("Author".into()),
                ..Person::default()
            }],
            journal: Some("Prior Journal".into()),
            first_page: Some("10".into()),
            last_page: Some("15".into()),
            page_range: Some("10–15".into()),
            date: Some(Date {
                raw: "February 29, 2024".into(),
                ..date(2024, Some(2), Some(29))
            }),
            doi: Some("10.1000/a&b".into()),
            raw_citation: "Raw <reference>".into(),
            bboxes: vec![bbox],
            ..BibItem::default()
        }],
        footnotes: vec![Footnote {
            id: Some("foot_1".into()),
            label: Some("1".into()),
            text: "Footnote".into(),
            page: Some(2),
        }],
        acknowledgements: Some("Thank you.".into()),
        annexes: vec![Section {
            title: Some("Supplement".into()),
            ..Section::default()
        }],
        page_count: 3,
    }
}

#[test]
fn header_encodes_book_isbn_editors_and_institutional_authors() {
    let header = Header {
        isbn: Some("9780387324586".into()),
        authors: vec![Person {
            organization: Some("Example Research Group".into()),
            ..Default::default()
        }],
        editors: vec![Person {
            first_name: Some("Jonathan".into()),
            last_name: Some("Turner".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let tei = header_to_tei_xml(&header, &TeiOptions::default()).unwrap();
    assert!(tei.contains("<idno type=\"ISBN\">9780387324586</idno>"));
    assert!(tei.contains("<orgName>Example Research Group</orgName>"));
    assert!(tei.contains("<editor>"));
    assert!(tei.contains("<surname>Turner</surname>"));
    let old: Header = serde_json::from_str(r#"{"title":"Book"}"#).unwrap();
    assert!(old.editors.is_empty() && old.isbn.is_none());
}
