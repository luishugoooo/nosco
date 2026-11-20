use std::collections::BTreeMap;
use std::io::{self, Write};

use quick_xml::events::Event;
use quick_xml::{Reader as XmlReader, XmlVersion, escape::resolve_predefined_entity};

use crate::cache::{Reader, U32s, invalid};
use crate::word_set::WordSet;
use crate::{java_trim, lowercase};

#[derive(Default)]
pub(crate) struct CountryMap {
    names: WordSet,
    codes: WordSet,
    code_ids: U32s,
}

#[derive(Clone, Copy, Default)]
enum Cell {
    #[default]
    Other,
    Code,
    Name,
}

fn xml_error(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

impl CountryMap {
    pub(crate) fn from_xml(xml: &str) -> io::Result<Self> {
        let mut reader = XmlReader::from_str(xml);
        reader.config_mut().expand_empty_elements = true;
        let mut entries = BTreeMap::new();
        let mut row_code = None;
        let mut cell = Cell::Other;
        let mut value = String::new();
        loop {
            match reader.read_event().map_err(xml_error)? {
                Event::Start(event) if event.name().as_ref() == "cell" => {
                    value.clear();
                    cell = Cell::Other;
                    for attr in event.attributes() {
                        let attr = attr.map_err(xml_error)?;
                        if attr.key.as_ref() == "role" {
                            cell = match attr
                                .normalized_value(XmlVersion::Implicit1_0)
                                .map_err(xml_error)?
                                .as_ref()
                            {
                                "a2code" => Cell::Code,
                                "name" | "nameAlt" => Cell::Name,
                                _ => Cell::Other,
                            };
                        }
                    }
                }
                Event::Text(text) if !matches!(cell, Cell::Other) => {
                    value.push_str(&text.xml10_content());
                }
                Event::CData(text) if !matches!(cell, Cell::Other) => {
                    value.push_str(&text.xml10_content());
                }
                Event::GeneralRef(entity) if !matches!(cell, Cell::Other) => {
                    if let Some(ch) = entity.resolve_char_ref().map_err(xml_error)? {
                        value.push(ch);
                    } else {
                        let text = resolve_predefined_entity(entity.as_ref())
                            .ok_or_else(|| invalid("unrecognized country XML entity"))?;
                        value.push_str(text);
                    }
                }
                Event::End(event) if event.name().as_ref() == "cell" => {
                    let text = java_trim(&value);
                    match cell {
                        Cell::Code => row_code = Some(text.to_owned()),
                        Cell::Name => {
                            entries.insert(lowercase(text).into_owned(), row_code.clone());
                        }
                        Cell::Other => {}
                    }
                    cell = Cell::Other;
                }
                Event::End(event) if event.name().as_ref() == "row" => row_code = None,
                Event::Eof => break,
                _ => {}
            }
        }

        let mut countries = Self::default();
        for (name, code) in entries {
            countries.names.intern(&name);
            countries.code_ids.push(
                code.as_deref()
                    .map_or(0, |code| countries.codes.intern(code) + 1),
            );
        }
        countries.names.shrink_to_fit();
        countries.codes.shrink_to_fit();
        countries.code_ids.shrink_to_fit();
        Ok(countries)
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.names.contains(&lowercase(name))
    }

    pub(crate) fn code(&self, name: &str) -> Option<&str> {
        let id = self.names.find(&lowercase(name))?;
        let code = self.code_ids.get(id as usize).checked_sub(1)?;
        Some(self.codes.get(code))
    }

    pub(crate) fn read(reader: &mut Reader) -> io::Result<Self> {
        let names = WordSet::read(reader)?;
        let codes = WordSet::read(reader)?;
        let code_ids = U32s::read(reader)?;
        if code_ids.len() != names.len() || code_ids.iter().any(|id| id as usize > codes.len()) {
            return Err(invalid("invalid country map"));
        }
        Ok(Self {
            names,
            codes,
            code_ids,
        })
    }

    pub(crate) fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        self.names.write(writer)?;
        self.codes.write(writer)?;
        self.code_ids.write(writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_alternate_names_and_preserves_code_lengths() {
        let map = CountryMap::from_xml(
            r#"
            <table><row>
                <cell role="a2code"> DDDE </cell>
                <cell role="a3code">ignored</cell>
                <cell role="name"> Germany &amp; Friends </cell>
                <cell role="nameAlt">östlich&#x65;s Deutschland</cell>
            </row><row>
                <cell role="name">uncoded</cell>
            </row></table>
        "#,
        )
        .unwrap();
        assert_eq!(map.code("GERMANY & FRIENDS"), Some("DDDE"));
        assert_eq!(map.code("Östliches Deutschland"), Some("DDDE"));
        assert!(map.contains("uncoded"));
        assert_eq!(map.code("uncoded"), None);
        assert!(!map.contains("ignored"));
    }
}
