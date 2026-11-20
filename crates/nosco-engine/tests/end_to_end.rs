use std::path::PathBuf;
use std::time::Instant;

use nosco_engine::{Engine, convert_grobid_home};
use nosco_models::TeiOptions;

fn scratch_grobid_home() -> Option<PathBuf> {
    let p = std::env::var_os("GROBID_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("NOSCO_GROBID_REF").map(|r| PathBuf::from(r).join("grobid-home"))
        })?;
    p.is_dir().then_some(p)
}

fn scratch_test_pdfs() -> Option<PathBuf> {
    let p = std::env::var_os("NOSCO_GROBID_REF")
        .map(|r| PathBuf::from(r).join("grobid-core/src/test/resources/test"))?;
    p.is_dir().then_some(p)
}

#[test]
fn end_to_end_conversion_raw_parsers_and_real_pdfs() {
    let Some(grobid_home) = scratch_grobid_home() else {
        eprintln!("skipping end_to_end test: reference grobid-home not present");
        return;
    };

    let compact_dir = std::env::temp_dir().join("nosco_e2e_assets");
    let _ = std::fs::remove_dir_all(&compact_dir);

    let t0 = Instant::now();
    let report = convert_grobid_home(&grobid_home, &compact_dir).expect("convert grobid-home");
    let convert_ms = t0.elapsed().as_millis();
    assert_eq!(report.models.len(), 11);
    assert!(report.total_output_bytes < report.total_input_bytes / 5);
    eprintln!(
        "Converted 11 models + lexicon in {convert_ms} ms: models_in={:.1} MB -> compact_total={:.1} MB (models_only={:.2} MB, lexicon={:.2} MB)",
        report.total_input_bytes as f64 / 1_048_576.0,
        report.total_output_bytes as f64 / 1_048_576.0,
        (report.total_output_bytes - report.lexicon_output_bytes) as f64 / 1_048_576.0,
        report.lexicon_output_bytes as f64 / 1_048_576.0,
    );

    let t1 = Instant::now();
    let engine = Engine::load(&compact_dir).expect("load compact engine");
    eprintln!("Loaded compact engine in {} ms", t1.elapsed().as_millis());

    // 1. Date parser
    let dates = engine.parse_date("March 14, 2023").expect("parse date");
    assert!(!dates.is_empty(), "expected parsed date");
    assert_eq!(dates[0].year, Some(2023));
    assert_eq!(dates[0].month, Some(3));
    assert_eq!(dates[0].day, Some(14));

    // 2. Header names parser
    let names = engine
        .parse_header_names("John A. Smith and Marie Curie")
        .expect("parse header names");
    assert_eq!(names.len(), 2, "names={names:?}");
    assert_eq!(names[0].last_name.as_deref(), Some("Smith"));
    assert_eq!(names[1].last_name.as_deref(), Some("Curie"));

    // 3. Citation names parser
    let cit_names = engine
        .parse_citation_names("A. Vaswani, N. Shazeer, and N. Parmar")
        .expect("parse citation names");
    assert!(cit_names.len() >= 3, "cit_names={cit_names:?}");
    assert_eq!(cit_names[0].last_name.as_deref(), Some("Vaswani"));

    // 4. Affiliation parser
    let affs = engine
        .parse_affiliations("Department of Computer Science, Stanford University, Stanford, CA, USA")
        .expect("parse affiliations");
    assert!(!affs.is_empty(), "affs={affs:?}");
    assert!(
        affs[0]
            .institutions
            .iter()
            .any(|i| i.to_lowercase().contains("stanford")),
        "affs={affs:?}"
    );
    assert_eq!(
        affs[0]
            .address
            .as_ref()
            .and_then(|a| a.country_code.as_deref()),
        Some("US")
    );

    // 5. Citation parser
    let bib = engine
        .parse_citation(
            "A. Vaswani, N. Shazeer, N. Parmar, Attention Is All You Need, Advances in Neural Information Processing Systems, vol. 30, pp. 5998-6008, 2017. doi:10.48550/arXiv.1706.03762",
        )
        .expect("parse citation");
    assert!(
        bib.title
            .as_deref()
            .unwrap_or("")
            .to_lowercase()
            .contains("attention"),
        "bib={bib:?}"
    );
    assert_eq!(bib.date.as_ref().and_then(|d| d.year), Some(2017));
    assert!(!bib.authors.is_empty(), "bib={bib:?}");
    assert_eq!(bib.doi.as_deref(), Some("10.48550/arXiv.1706.03762"));

    // 6. Full PDF pipeline on real scholarly PDFs
    if let Some(pdf_dir) = scratch_test_pdfs() {
        for pdf_name in [
            "Wang-paperAVE2008.pdf",
            "MullenJSSv18i03.pdf",
            "0807.3577.pdf",
            "test_Grobid_1_05452615.pdf",
        ] {
            let pdf_path = pdf_dir.join(pdf_name);
            if !pdf_path.exists() {
                continue;
            }
            let tp = Instant::now();
            let doc = engine.process_pdf_file(&pdf_path).expect("process pdf");
            let dt = tp.elapsed().as_millis();
            eprintln!(
                "{pdf_name}: {dt}ms | pages={} | title={:?} | authors={} | affs={} | sections={} | refs={} | figs={} | tabs={}",
                doc.page_count,
                doc.header.title,
                doc.header.authors.len(),
                doc.header.affiliations.len(),
                doc.body.len(),
                doc.references.len(),
                doc.figures.len(),
                doc.tables.len(),
            );
            assert!(doc.page_count > 0);
            assert!(
                doc.header.title.as_deref().is_some_and(|t| t.len() > 8),
                "{pdf_name}: missing title, header={:?}",
                doc.header
            );
            assert!(
                !doc.header.authors.is_empty(),
                "{pdf_name}: missing authors, header={:?}",
                doc.header
            );
            assert!(
                !doc.references.is_empty(),
                "{pdf_name}: missing references"
            );

            let tei = doc.to_tei_xml(&TeiOptions::default()).expect("tei xml");
            assert!(tei.contains("<TEI "));
            assert!(tei.contains("</TEI>"));
            let json = doc.to_json().expect("json");
            assert!(json.starts_with('{'));
        }
    }

    let _ = std::fs::remove_dir_all(&compact_dir);
}
