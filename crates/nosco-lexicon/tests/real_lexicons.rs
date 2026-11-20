use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use nosco_lexicon::{Lexicon, tokenizer::tokenize};

fn reference_home() -> Option<PathBuf> {
    std::env::var_os("GROBID_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("NOSCO_GROBID_REF").map(|p| PathBuf::from(p).join("grobid-home")))
}

type Lookup = fn(&Lexicon, &[&str]) -> Vec<(usize, usize)>;

fn assert_parity(source: &Lexicon, cached: &Lexicon) {
    for lexicon in [source, cached] {
        assert!(lexicon.in_dictionary("WELL-KNOWN;"));
        assert!(lexicon.in_dictionary("Don't"));
        assert!(lexicon.in_first_names("Mary"));
        assert!(lexicon.in_last_names("Smith"));
        assert!(lexicon.in_countries("South Africa"));
        assert!(lexicon.is_country("Deutschland"));
        assert_eq!(lexicon.country_code("Afrique du Sud"), Some("ZA"));
        assert_eq!(lexicon.country_code("not a country"), None);
        assert!(lexicon.in_city("New York"));
        assert!(lexicon.in_person_title("Professor Emeritus"));
        assert!(lexicon.in_person_suffix("Jr."));
    }

    let cases: &[(Lookup, &str, usize)] = &[
        (Lexicon::in_city_positions, "New York", 1),
        (
            Lexicon::in_journal_positions,
            "ACS Applied Materials & Interfaces",
            4,
        ),
        (
            Lexicon::in_abbrev_journal_positions,
            "ACS Appl. Mater. Interfaces",
            5,
        ),
        (Lexicon::in_conference_positions, "SPIE Conference", 1),
        (Lexicon::in_publisher_positions, "AACN PUBLISHING", 1),
        (
            Lexicon::in_organisation_positions,
            "American National Standards Institute",
            3,
        ),
        (
            Lexicon::in_organisation_positions,
            "United States Information Agency",
            3,
        ),
        (Lexicon::in_organisation_positions, "1-800-FLOWERS.COM", 6),
        (
            Lexicon::in_organisation_positions,
            "supernus pharmaceuticals",
            1,
        ),
        (Lexicon::in_org_form_positions, "corp.", 0),
        (Lexicon::in_location_positions, "Bronx County", 1),
        (Lexicon::in_collaboration_positions, "ATLAS", 0),
        (Lexicon::in_person_title_positions, "Professor Emeritus", 1),
        (Lexicon::in_person_suffix_positions, "Jr.", 0),
    ];
    for &(lookup, text, end) in cases {
        let tokens = tokenize(text);
        let tokens = tokens.iter().map(String::as_str).collect::<Vec<_>>();
        let expected = lookup(source, &tokens);
        assert!(expected.contains(&(0, end)), "{text}: {expected:?}");
        assert_eq!(lookup(cached, &tokens), expected, "{text}");
    }
}

#[test]
fn real_lexicons_roundtrip_and_cache_startup() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Lexicon>();

    let Some(home) = reference_home().filter(|p| p.is_dir()) else {
        eprintln!("Skipping real gazetteer test because GROBID_HOME / NOSCO_GROBID_REF is unset or absent");
        return;
    };

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let dir =
        std::env::temp_dir().join(format!("nosco-lexicon-integration-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let dir = TempDir(dir);
    let cache = dir.0.join("lexicon.glex");

    let start = Instant::now();
    let source = Lexicon::load_from_grobid_home(&home).unwrap();
    let source_elapsed = start.elapsed();
    source.save_glex(&cache).unwrap();

    let start = Instant::now();
    let cached = Lexicon::load_glex(&cache).unwrap();
    let cache_elapsed = start.elapsed();

    assert_parity(&source, &cached);
    eprintln!(
        "Real gazetteers: source {source_elapsed:?}, cache {cache_elapsed:?}, {} bytes",
        fs::metadata(&cache).unwrap().len()
    );
    assert!(
        cache_elapsed < source_elapsed,
        "loading the cache should be faster than parsing the source files"
    );
}
