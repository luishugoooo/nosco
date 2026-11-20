//! GROBID gazetteers, an immutable multi-token matcher, and the default analyzer.

mod cache;
mod country;
mod matcher;
pub mod tokenizer;
mod word_set;

use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use country::CountryMap;
use matcher::MatcherBuilder;
use word_set::WordSet;

pub use matcher::FastMatcher;

const MAGIC: &[u8; 4] = b"GLEX";
const VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum LexiconError {
    #[error("{path}: {source}")]
    Resource {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl LexiconError {
    fn at(path: &Path, source: io::Error) -> Self {
        Self::Resource {
            path: path.to_owned(),
            source,
        }
    }
}

// The order of fields in this declaration is the section order in GLEX v1.
macro_rules! lexicon {
    (
        words { $( $words:ident, )* }
        countries: CountryMap,
        $( $matcher:ident : FastMatcher => $positions:ident, )*
    ) => {
        /// GROBID's English dictionary and core gazetteers, immutable after loading.
        #[derive(Default)]
        pub struct Lexicon {
            $( $words: WordSet, )*
            countries: CountryMap,
            $( $matcher: FastMatcher, )*
        }

        impl Lexicon {
            $(
                /// Finds matching gazetteer entries as inclusive token index pairs.
                pub fn $positions(&self, tokens: &[&str]) -> Vec<(usize, usize)> {
                    self.$matcher.match_tokens(tokens)
                }
            )*

            fn read_cache(reader: &mut cache::Reader) -> io::Result<Self> {
                Ok(Self {
                    $( $words: WordSet::read(reader)?, )*
                    countries: CountryMap::read(reader)?,
                    $( $matcher: FastMatcher::read(reader)?, )*
                })
            }

            fn write_cache(&self, writer: &mut impl Write) -> io::Result<()> {
                writer.write_all(MAGIC)?;
                writer.write_all(&VERSION.to_le_bytes())?;
                $( self.$words.write(writer)?; )*
                self.countries.write(writer)?;
                $( self.$matcher.write(writer)?; )*
                Ok(())
            }
        }
    };
}

lexicon! {
    words { dictionary, first_names, last_names, }
    countries: CountryMap,
    cities: FastMatcher => in_city_positions,
    journals: FastMatcher => in_journal_positions,
    abbreviated_journals: FastMatcher => in_abbrev_journal_positions,
    conferences: FastMatcher => in_conference_positions,
    publishers: FastMatcher => in_publisher_positions,
    organisations: FastMatcher => in_organisation_positions,
    org_forms: FastMatcher => in_org_form_positions,
    locations: FastMatcher => in_location_positions,
    collaborations: FastMatcher => in_collaboration_positions,
    person_titles: FastMatcher => in_person_title_positions,
    person_suffixes: FastMatcher => in_person_suffix_positions,
}

impl Lexicon {
    /// Returns a lexicon with no entries.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Loads the core gazetteers from the `lexicon` directory of a GROBID home.
    pub fn load_from_grobid_home(grobid_home: &Path) -> Result<Self, LexiconError> {
        let root = grobid_home.join("lexicon");
        let country_path = root.join("countries/CountryCodes.xml");
        let xml =
            fs::read_to_string(&country_path).map_err(|e| LexiconError::at(&country_path, e))?;
        Ok(Self {
            dictionary: read_words(&root, &["wordforms/english.wf"], |line| {
                // Java's Character.isWhitespace excludes non-breaking spaces.
                if line.chars().all(|ch| {
                    matches!(ch, '\x1c'..='\x1f')
                        || (ch.is_whitespace()
                            && !matches!(ch, '\u{85}' | '\u{a0}' | '\u{2007}' | '\u{202f}'))
                }) {
                    None
                } else {
                    first_field(line, &['\t'])
                }
            })?,
            first_names: read_words(
                &root,
                &[
                    "names/names.female",
                    "names/names.male",
                    "names/firstname.5k",
                ],
                name_field,
            )?,
            last_names: read_words(
                &root,
                &["names/names.family", "names/lastname.5k"],
                name_field,
            )?,
            countries: CountryMap::from_xml(&xml)
                .map_err(|e| LexiconError::at(&country_path, e))?,
            cities: read_phrases(&root, &["places/cities15000.txt"])?,
            journals: read_phrases(&root, &["journals/journals.txt"])?,
            abbreviated_journals: read_phrases(&root, &["journals/abbrev_journals.txt"])?,
            conferences: read_phrases(&root, &["journals/proceedings.txt"])?,
            publishers: read_phrases(&root, &["publishers/publishers.txt"])?,
            organisations: read_phrases(
                &root,
                &[
                    "organisations/WikiOrganizations.lst",
                    "organisations/government.government_agency",
                    "organisations/known_corporations.lst",
                    "organisations/venture_capital.venture_funded_company",
                ],
            )?,
            org_forms: read_phrases(&root, &["organisations/orgClosings.txt"])?,
            locations: read_phrases(&root, &["places/location.txt"])?,
            collaborations: read_phrases(&root, &["organisations/inspire_collaborations.txt"])?,
            person_titles: read_phrases(&root, &["names/VincentNgPeopleTitles.txt"])?,
            person_suffixes: read_phrases(&root, &["names/suffix.txt"])?,
        })
    }

    /// Writes the v1 binary cache. An existing cache is replaced after a successful write.
    pub fn save_glex(&self, path: &Path) -> Result<(), LexiconError> {
        write_atomically(path, |file| {
            let mut writer = BufWriter::new(file);
            self.write_cache(&mut writer)?;
            writer.flush()
        })
        .map_err(|e| LexiconError::at(path, e))
    }

    /// Loads the binary cache directly into its packed lookup arrays.
    pub fn load_glex(path: &Path) -> Result<Self, LexiconError> {
        let load = || {
            let mut reader = cache::Reader::open(path)?;
            let mut header = [0; 8];
            reader.read(&mut header)?;
            if &header[..4] != MAGIC {
                return Err(cache::invalid("invalid GLEX magic"));
            }
            if header[4..] != VERSION.to_le_bytes() {
                return Err(cache::invalid("unsupported GLEX version"));
            }
            let lexicon = Self::read_cache(&mut reader)?;
            reader.finish()?;
            Ok(lexicon)
        };
        load().map_err(|e| LexiconError::at(path, e))
    }

    /// Looks up an English wordform. A compound split at the first hyphen (or
    /// first space if there is no hyphen) requires both halves to be present.
    /// Apostrophes are literal, as in the dictionary's contraction entries.
    pub fn in_dictionary(&self, word: &str) -> bool {
        let word = word.strip_suffix(['.', ',', ':', ';']).unwrap_or(word);
        let word = lowercase(word);
        match word.split_once('-').or_else(|| word.split_once(' ')) {
            Some((left, right)) => {
                self.dictionary.contains(left) && self.dictionary.contains(right)
            }
            None => self.dictionary.contains(&word),
        }
    }

    pub fn in_first_names(&self, word: &str) -> bool {
        self.first_names.contains(&lowercase(word))
    }

    pub fn in_last_names(&self, word: &str) -> bool {
        self.last_names.contains(&lowercase(word))
    }

    pub fn in_countries(&self, word: &str) -> bool {
        self.is_country(word)
    }

    pub fn country_code(&self, name: &str) -> Option<&str> {
        self.countries.code(name)
    }

    /// Tests an entire country name or alias from CountryCodes.xml.
    pub fn is_country(&self, text: &str) -> bool {
        self.countries.contains(text)
    }

    pub fn in_city(&self, word: &str) -> bool {
        self.cities.contains_phrase(word)
    }

    pub fn in_person_title(&self, word: &str) -> bool {
        self.person_titles.contains_phrase(word)
    }

    pub fn in_person_suffix(&self, word: &str) -> bool {
        self.person_suffixes.contains_phrase(word)
    }
}

fn first_field<'a>(line: &'a str, delimiters: &[char]) -> Option<&'a str> {
    line.split(delimiters).find(|part| !part.is_empty())
}

fn name_field(line: &str) -> Option<&str> {
    first_field(line, &['\t', '-']).map(java_trim)
}

fn java_trim(text: &str) -> &str {
    text.trim_matches(|ch| ch <= ' ')
}

fn lowercase(text: &str) -> Cow<'_, str> {
    if text.is_ascii() && !text.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(text.to_lowercase())
    }
}

fn for_each_line(
    root: &Path,
    sources: &[&str],
    mut read_line: impl FnMut(&str),
) -> Result<(), LexiconError> {
    for source in sources {
        let path = root.join(source);
        let bytes = fs::read(&path).map_err(|e| LexiconError::at(&path, e))?;
        // Java's UTF-8 reader replaces malformed input in the shipped text lists.
        for line in String::from_utf8_lossy(&bytes).split_terminator(['\r', '\n']) {
            if !line.is_empty() {
                read_line(line);
            }
        }
    }
    Ok(())
}

fn read_words(
    root: &Path,
    sources: &[&str],
    field: impl Fn(&str) -> Option<&str>,
) -> Result<WordSet, LexiconError> {
    let mut words = WordSet::default();
    for_each_line(root, sources, |line| {
        if let Some(word) = field(line) {
            words.intern(&lowercase(word));
        }
    })?;
    words.shrink_to_fit();
    Ok(words)
}

fn read_phrases(root: &Path, sources: &[&str]) -> Result<FastMatcher, LexiconError> {
    let mut builder = MatcherBuilder::default();
    for_each_line(root, sources, |line| builder.add(line))?;
    Ok(builder.finish())
}

fn write_atomically(path: &Path, write: impl FnOnce(File) -> io::Result<()>) -> io::Result<()> {
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    let (temporary, file) = loop {
        let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let temporary = path.with_file_name(format!(".glex-{}-{id}.tmp", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = write(file).and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_preserves_contractions_and_checks_both_compound_parts() {
        let mut lexicon = Lexicon::empty();
        for word in ["word", "form", "don't", "'s", "one-sided"] {
            lexicon.dictionary.intern(word);
        }
        assert!(lexicon.in_dictionary("WORD-FORM,"));
        assert!(lexicon.in_dictionary("word form:"));
        assert!(lexicon.in_dictionary("Don't."));
        assert!(lexicon.in_dictionary("'s"));
        assert!(!lexicon.in_dictionary("don’t"));
        assert!(!lexicon.in_dictionary("word-unknown"));
        assert!(!lexicon.in_dictionary("one-sided"));
        assert!(!lexicon.in_dictionary("word.."));
        assert_eq!(name_field("\t--  Anne-Marie\t42"), Some("Anne"));
    }

    #[test]
    fn glex_roundtrip_and_rejects_invalid_cache_data() {
        let dir = std::env::temp_dir().join(format!("nosco-lexicon-unit-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let dir = Cleanup(dir);
        let path = dir.0.join("lexicon.glex");
        let mut source = Lexicon::empty();
        source.save_glex(&path).unwrap();
        assert!(!Lexicon::load_glex(&path).unwrap().in_dictionary("word"));

        source.dictionary.intern("word");
        source.first_names.intern("héloïse");
        source.last_names.intern("smith");
        source.countries = CountryMap::from_xml(
            r#"<row><cell role="a2code">FR</cell><cell role="name">France</cell></row>"#,
        )
        .unwrap();
        source.cities = FastMatcher::from_phrases(["New York", "New York City"]);
        source.save_glex(&path).unwrap();

        let cached = Lexicon::load_glex(&path).unwrap();
        assert!(cached.in_dictionary("word"));
        assert!(cached.in_first_names("HÉLOÏSE"));
        assert!(cached.in_last_names("Smith"));
        assert_eq!(cached.country_code("France"), Some("FR"));
        assert_eq!(
            cached.in_city_positions(&["New", "York", "City"]),
            [(0, 1), (0, 2)]
        );
        assert!(cached.in_journal_positions(&["Something"]).is_empty());

        let valid = fs::read(&path).unwrap();
        assert_eq!(&valid[..8], b"GLEX\x01\0\0\0");
        let second_path = dir.0.join("roundtrip.glex");
        cached.save_glex(&second_path).unwrap();
        assert_eq!(valid, fs::read(second_path).unwrap());
        for (offset, byte) in [(0, b'X'), (4, 2), (8, 255), (24, 255)] {
            let mut corrupt = valid.clone();
            corrupt[offset] = byte;
            fs::write(&path, corrupt).unwrap();
            assert!(Lexicon::load_glex(&path).is_err(), "offset {offset}");
        }
        fs::write(&path, &valid[..valid.len() - 1]).unwrap();
        assert!(Lexicon::load_glex(&path).is_err());
        fs::write(&path, [&valid[..], &[0]].concat()).unwrap();
        assert!(Lexicon::load_glex(&path).is_err());
    }
}
