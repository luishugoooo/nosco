use std::collections::BTreeMap;

use quick_xml::{Reader, events::Event};

const FIELDS: &[&str] = &[
    "prism:doi",
    "pdfx:doi",
    "crossmark:DOI",
    "dc:identifier",
    "prism:publicationName",
    "prism:volume",
    "prism:number",
    "prism:startingPage",
    "prism:endingPage",
    "prism:isbn",
    "prism:issn",
    "prism:eIssn",
    "dc:title",
    "dc:creator",
    "dc:publisher",
    "dc:date",
    "dc:language",
    "xmp:CreateDate",
    "prism:coverDate",
    "prism:publicationDate",
];

pub(crate) fn parse_xmp(xml: &[u8]) -> BTreeMap<String, Vec<String>> {
    let mut reader = Reader::from_reader(xml);
    let mut fields = BTreeMap::<String, Vec<String>>::new();
    let mut stack = Vec::new();
    let mut text = String::new();
    let mut save = |key: &str, value: &str| {
        let value = value.trim();
        if !value.is_empty() {
            fields
                .entry(key.to_owned())
                .or_default()
                .push(value.to_owned());
        }
    };
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                for attr in element.attributes().flatten() {
                    if let Some(key) = field(attr.key.as_ref())
                        && let Ok(value) = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    {
                        save(key, &value);
                    }
                }
                if matches!(event, Event::Start(_)) {
                    let name = element.name();
                    if field(name.as_ref()).is_some() || name.as_ref() == "rdf:li" {
                        text.clear();
                    }
                    stack.push(field(name.as_ref()));
                }
            }
            Event::Text(value) if stack.iter().any(Option::is_some) => {
                text.push_str(&value.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Event::CData(value) if stack.iter().any(Option::is_some) => {
                text.push_str(&value.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Event::GeneralRef(value) if stack.iter().any(Option::is_some) => {
                if let Ok(Some(ch)) = value.resolve_char_ref() {
                    text.push(ch);
                } else if let Some(value) = quick_xml::escape::resolve_xml_entity(
                    &value.xml_content(quick_xml::XmlVersion::Implicit1_0),
                ) {
                    text.push_str(value);
                }
            }
            Event::End(element) => {
                if let Some(Some(key)) = stack.pop() {
                    save(key, &text);
                    text.clear();
                } else if element.name().as_ref() == "rdf:li" {
                    if let Some(key) = stack.iter().rev().flatten().next() {
                        save(key, &text);
                    }
                    text.clear();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    fields
}

fn field(name: &str) -> Option<&'static str> {
    FIELDS.iter().copied().find(|key| *key == name)
}

#[test]
fn reads_xmp_attributes_entities_and_ordered_creators() {
    let fields = parse_xmp(br#"<rdf:Description prism:doi="10.1000/a1"><dc:title><rdf:Alt><rdf:li>A &amp; B</rdf:li></rdf:Alt></dc:title><dc:creator><rdf:Seq><rdf:li>First Author</rdf:li><rdf:li>Second Author</rdf:li></rdf:Seq></dc:creator><prism:number>7</prism:number></rdf:Description>"#);
    assert_eq!(fields["prism:doi"], ["10.1000/a1"]);
    assert_eq!(fields["dc:title"], ["A & B"]);
    assert_eq!(fields["dc:creator"], ["First Author", "Second Author"]);
    assert_eq!(fields["prism:number"], ["7"]);
}
