use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use nosco_features::*;
use nosco_lexicon::{Lexicon, tokenizer};
use nosco_pdf::{
    BoundingBox, GraphicKind, GraphicObject, LayoutBlock, LayoutDocument, LayoutLine, LayoutToken,
    PageInfo,
};

fn model_home() -> PathBuf {
    std::env::var_os("GROBID_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("NOSCO_GROBID_REF").map(|r| PathBuf::from(r).join("grobid-home"))
        })
        .unwrap_or_default()
}

fn assert_matrix(matrix: &FeatureMatrix, width: usize) {
    assert!(!matrix.is_empty());
    assert_eq!(matrix.num_cols(), width);
    assert_eq!(matrix.len(), matrix.as_rows().len());
    for (row, wapiti) in matrix.as_rows().iter().zip(matrix.to_wapiti_lines()) {
        assert_eq!(row.len(), width, "{row:?}");
        assert!(
            row.iter()
                .all(|cell| !cell.is_empty() && !cell.contains(char::is_whitespace))
        );
        assert_eq!(wapiti.split_whitespace().collect::<Vec<_>>(), *row);
    }
}

const LINE: &[&str] = &["LINESTART", "LINEIN", "LINEEND"];
const BLOCK: &[&str] = &["BLOCKSTART", "BLOCKIN", "BLOCKEND"];
const PAGE: &[&str] = &["PAGESTART", "PAGEIN", "PAGEEND"];
const ALIGN: &[&str] = &["ALIGNEDLEFT", "LINEINDENT"];
const FONT: &[&str] = &["NEWFONT", "SAMEFONT"];
const SIZE: &[&str] = &["HIGHERFONT", "SAMEFONTSIZE", "LOWERFONT"];
const CAPS: &[&str] = &["ALLCAP", "INITCAP", "NOCAPS"];
const DIGITS: &[&str] = &["ALLDIGIT", "CONTAINSDIGITS", "NODIGIT"];
const PUNCT: &[&str] = &[
    "OPENBRACKET",
    "ENDBRACKET",
    "DOT",
    "COMMA",
    "HYPHEN",
    "QUOTE",
    "PUNCT",
    "NOPUNCT",
];

// Stable categorical observations and numeric ranges from the Wapiti templates
// and the corresponding Java row layouts. Older templates may contain unused
// historical observations, such as NOPUNCT at citation's position column.
fn assert_model_vocabulary(model: &str, matrix: &FeatureMatrix) {
    for row in matrix.as_rows() {
        let values = |domains: &[(usize, &[&str])]| {
            for &(i, domain) in domains {
                assert!(
                    domain.contains(&row[i].as_str()),
                    "{model} column {i}: {:?}",
                    row[i]
                );
            }
        };
        let bits = |range: std::ops::Range<usize>| {
            for i in range {
                assert!(
                    matches!(row[i].as_str(), "0" | "1"),
                    "{model} column {i}: {:?}",
                    row[i]
                );
            }
        };
        let bins = |columns: &[usize], max: usize| {
            for &i in columns {
                assert!(
                    row[i].parse::<usize>().is_ok_and(|n| n <= max),
                    "{model} column {i}: {:?}",
                    row[i]
                );
            }
        };
        match model {
            "date" => {
                values(&[
                    (10, LINE),
                    (11, CAPS),
                    (12, DIGITS),
                    (16, PUNCT),
                    (17, &["<date>"]),
                ]);
                bits(13..16);
            }
            "name/citation" | "name/header" => {
                values(&[
                    (10, LINE),
                    (11, CAPS),
                    (12, DIGITS),
                    (19, PUNCT),
                    (20, &["0"]),
                ]);
                bits(13..19);
            }
            "affiliation-address" => {
                values(&[
                    (10, LINE),
                    (11, &["ALLCAPS", "INITCAP", "NOCAPS"]),
                    (12, &["ALLDIGIT", "CONTAINDIGIT", "NODIGIT"]),
                    (19, PUNCT),
                    (21, &["<affiliation>"]),
                ]);
                bits(13..19);
                assert_eq!(row[20], factory::word_shape(&row[0]));
            }
            "citation" => {
                values(&[
                    (10, LINE),
                    (11, CAPS),
                    (12, DIGITS),
                    (27, PUNCT),
                    (29, &["0"]),
                ]);
                bits(13..27);
                bins(&[28], 12);
            }
            "reference-segmenter" => {
                values(&[
                    (10, LINE),
                    (11, ALIGN),
                    (12, CAPS),
                    (13, DIGITS),
                    (22, PUNCT),
                    (25, BLOCK),
                    (27, &["0"]),
                ]);
                bits(14..22);
                bins(&[23, 24], 10);
                if row[26] != "no" {
                    assert!((1..=10).contains(&row[26].parse().unwrap()));
                }
            }
            "segmentation" => {
                values(&[
                    (7, BLOCK),
                    (8, PAGE),
                    (9, FONT),
                    (10, SIZE),
                    (13, CAPS),
                    (14, DIGITS),
                ]);
                bits(11..13);
                bits(15..23);
                bits(28..33);
                bins(&[23, 24], 12);
                bins(&[27], 10);
                let profile_length = if row[25] == "no" {
                    0
                } else {
                    assert!(
                        row[25]
                            .chars()
                            .all(|ch| tokenizer::FULL_PUNCTUATIONS.contains(ch))
                    );
                    row[25].encode_utf16().count()
                };
                assert_eq!(row[26].parse::<usize>().unwrap(), profile_length);
            }
            "header" | "fulltext" | "figure" | "table" => {
                values(&[
                    (10, BLOCK),
                    (11, LINE),
                    (12, ALIGN),
                    (13, FONT),
                    (14, SIZE),
                    (17, CAPS),
                    (18, DIGITS),
                ]);
                bits(15..17);
                if model == "header" {
                    bits(19..27);
                    bits(28..31);
                    values(&[(27, PUNCT), (31, &["0"])]);
                } else {
                    values(&[(20, PUNCT), (24, &["NUMBER", "AUTHOR", "UNKNOWN"])]);
                    bits(19..20);
                    bits(23..24);
                    bits(25..27);
                    bins(&[21, 22], 12);
                }
            }
            _ => panic!("unknown model {model}"),
        }
    }
}

fn assert_row(matrix: &FeatureMatrix, index: usize, expected: &str) {
    assert_eq!(matrix.rows[index].join(" "), expected);
}

#[test]
fn text_model_rows() {
    let lexicon = Lexicon::empty();
    let date = extract_date_features(&["March", " ", "27", ",", "2009"], &lexicon);
    assert_matrix(&date, 18);
    assert_row(
        &date,
        0,
        "March march M Ma Mar Marc h ch rch arch LINESTART INITCAP NODIGIT 0 0 1 NOPUNCT <date>",
    );
    assert_row(
        &date,
        2,
        ", , , , , , , , , , LINEIN ALLCAP NODIGIT 1 0 0 COMMA <date>",
    );
    assert_row(
        &date,
        3,
        "2009 2009 2 20 200 2009 9 09 009 2009 LINEEND NOCAPS ALLDIGIT 0 1 0 NOPUNCT <date>",
    );

    let tokens = ["Dr", ".", " ", "\n", "Anne", " ", "LEE"];
    let names_citation = extract_name_citation_features(&tokens, &lexicon);
    assert_matrix(&names_citation, 21);
    assert_row(
        &names_citation,
        0,
        "Dr dr D Dr Dr Dr r Dr Dr Dr LINESTART INITCAP NODIGIT 0 0 0 0 0 0 NOPUNCT 0",
    );
    assert_row(
        &names_citation,
        1,
        ". . . . . . . . . . LINEEND ALLCAP NODIGIT 1 0 0 0 0 0 DOT 0",
    );
    assert_eq!(names_citation.rows[2][10], "LINEIN");
    assert_row(
        &names_citation,
        3,
        "LEE lee L LE LEE LEE E EE LEE LEE LINEEND ALLCAP NODIGIT 0 0 0 0 0 0 NOPUNCT 0",
    );

    let names_header =
        extract_name_header_features(&tokens, &[true, false, false, false, true], &lexicon);
    assert_matrix(&names_header, 21);
    assert_eq!(names_header.rows[1][10], "LINEIN");
    assert_eq!(names_header.rows[3], names_citation.rows[3]);

    let affiliation = extract_affiliation_features(
        &["Mental", "Health", "A1", "Lab"],
        &[true, false, true, false],
        &lexicon,
    );
    assert_matrix(&affiliation, 22);
    assert_row(
        &affiliation,
        0,
        "Mental mental M Me Men Ment l al tal ntal LINESTART INITCAP NODIGIT 0 0 0 0 0 0 NOPUNCT Xxxx <affiliation>",
    );
    assert_row(
        &affiliation,
        2,
        "A1 a1 A A1 A1 A1 1 A1 A1 A1 LINESTART ALLCAPS CONTAINDIGIT 0 0 0 0 0 0 NOPUNCT Xd <affiliation>",
    );
    assert_eq!(affiliation.rows[1][10], "LINEEND");
    assert_eq!(affiliation.rows[3][10], "LINEEND");

    let citation = extract_citation_features(
        &["Proc", ".", " ", "2008"],
        &[true, false, false, true],
        &lexicon,
    );
    assert_matrix(&citation, 30);
    assert_row(
        &citation,
        0,
        "Proc proc P Pr Pro Proc c oc roc Proc LINESTART INITCAP NODIGIT 0 0 0 0 0 0 0 0 0 0 0 0 0 0 NOPUNCT 0 0",
    );
    assert_row(
        &citation,
        2,
        "2008 2008 2 20 200 2008 8 08 008 2008 LINEEND NOCAPS ALLDIGIT 0 0 0 0 0 0 1 0 0 0 0 0 0 0 NOPUNCT 9 0",
    );

    let tokens = tokenizer::tokenize_with_spaces("A42b doi:10.1234/abc https://grobid.org");
    let tokens: Vec<_> = tokens.iter().map(String::as_str).collect();
    let citation = extract_citation_features(&tokens, &[], &lexicon);
    assert_eq!(
        &citation.rows[..3]
            .iter()
            .map(|row| row[0].as_str())
            .collect::<Vec<_>>(),
        &["A", "42", "b"]
    );
    assert!(
        citation
            .rows
            .iter()
            .any(|row| row[0] == "abc" && row[26] == "1")
    );
    assert!(
        citation
            .rows
            .iter()
            .any(|row| row[0] == "https" && row[21] == "1")
    );

    let reference = extract_reference_segmenter_features_text("[1] Ada\nRef.", &lexicon);
    assert_matrix(&reference, 28);
    assert_row(
        &reference,
        0,
        "[ [ [ [ [ [ [ [ [ [ LINESTART ALIGNEDLEFT ALLCAP NODIGIT 1 0 0 0 0 0 0 0 OPENBRACKET 0 10 BLOCKSTART 2 0",
    );
    assert_eq!(
        reference
            .rows
            .iter()
            .map(|row| row[0].as_str())
            .collect::<Vec<_>>(),
        ["[", "1", "]", "Ada", "Ref", "."]
    );
}

fn sample_layout() -> LayoutDocument {
    let bbox = BoundingBox::new;
    let tokenize = |idx: usize, text: &str, line, block, x, y, new_line_after: bool| LayoutToken {
        text: text.into(),
        page: 1,
        bbox: bbox(1, x, y, 10.0, 10.0),
        font_name: if idx < 4 { "Main" } else { "Small" }.into(),
        font_size: if idx < 4 { 12.0 } else { 10.0 },
        bold: idx < 2,
        italic: idx > 3,
        subscript: false,
        superscript: idx == 4,
        color: (0, 0, 0),
        rotated: false,
        space_after: !new_line_after,
        new_line_after,
        block_idx: block,
        line_idx: line,
    };
    LayoutDocument {
        pages: vec![PageInfo {
            number: 1,
            width: 100.0,
            height: 120.0,
            main_area: bbox(1, 5.0, 5.0, 90.0, 110.0),
            block_range: 0..2,
            line_range: 0..3,
            token_range: 0..6,
        }],
        blocks: vec![
            LayoutBlock {
                page: 1,
                bbox: bbox(1, 10.0, 10.0, 40.0, 30.0),
                token_range: 0..4,
                line_range: 0..2,
                block_idx: 0,
            },
            LayoutBlock {
                page: 1,
                bbox: bbox(1, 10.0, 50.0, 40.0, 10.0),
                token_range: 4..6,
                line_range: 2..3,
                block_idx: 1,
            },
        ],
        lines: vec![
            LayoutLine {
                page: 1,
                bbox: bbox(1, 10.0, 10.0, 20.0, 10.0),
                token_range: 0..2,
                block_idx: 0,
            },
            LayoutLine {
                page: 1,
                bbox: bbox(1, 30.0, 20.0, 20.0, 10.0),
                token_range: 2..4,
                block_idx: 0,
            },
            LayoutLine {
                page: 1,
                bbox: bbox(1, 10.0, 50.0, 20.0, 10.0),
                token_range: 4..6,
                block_idx: 1,
            },
        ],
        tokens: vec![
            tokenize(0, "Figure", 0, 0, 10.0, 10.0, false),
            tokenize(1, "1", 0, 0, 20.0, 10.0, true),
            tokenize(2, "March", 1, 0, 30.0, 20.0, false),
            tokenize(3, "2008", 1, 0, 40.0, 20.0, true),
            tokenize(4, "2", 2, 1, 10.0, 50.0, false),
            tokenize(5, ".", 2, 1, 20.0, 50.0, true),
        ],
        graphics: vec![
            GraphicObject {
                page: 1,
                bbox: bbox(1, 50.0, 70.0, 40.0, 30.0),
                kind: GraphicKind::Image,
            },
            GraphicObject {
                page: 1,
                bbox: bbox(1, 50.0, 70.0, 20.0, 1.0),
                kind: GraphicKind::HorizontalRule,
            },
        ],
        ..LayoutDocument::default()
    }
}

#[test]
fn layout_model_rows() {
    let doc = sample_layout();
    let all = [0, 1, 2, 3, 4, 5];
    let lexicon = Lexicon::empty();
    let (segmentation, lines) = extract_segmentation_features(&doc, &lexicon);
    assert_matrix(&segmentation, 33);
    assert_eq!(lines, [0, 1, 2]);
    assert_row(
        &segmentation,
        0,
        "Figure 1 figure F Fi Fig Figu BLOCKSTART PAGESTART NEWFONT HIGHERFONT 1 0 INITCAP NODIGIT 0 0 0 0 0 0 0 0 0 0 no 0 8 1 1 0 0 1",
    );

    let header = extract_header_features(&doc, &all, &lexicon);
    assert_matrix(&header, 32);
    assert_row(
        &header,
        0,
        "Figure figure F Fi Fig Figu e re ure gure BLOCKSTART LINESTART ALIGNEDLEFT NEWFONT HIGHERFONT 1 0 INITCAP NODIGIT 0 0 0 0 0 0 0 0 NOPUNCT 1 0 1 0",
    );
    assert_eq!(&header.rows[4][13..17], ["NEWFONT", "LOWERFONT", "0", "1"]);

    let fulltext = extract_fulltext_features(&doc, &all, &lexicon);
    assert_matrix(&fulltext, 27);
    assert_row(
        &fulltext,
        0,
        "Figure figure F Fi Fig Figu e re ure gure BLOCKSTART LINESTART ALIGNEDLEFT NEWFONT HIGHERFONT 1 0 INITCAP NODIGIT 0 NOPUNCT 0 1 1 UNKNOWN 0 0",
    );
    assert_eq!(
        &fulltext.rows[2][10..15],
        [
            "BLOCKIN",
            "LINESTART",
            "LINEINDENT",
            "SAMEFONT",
            "SAMEFONTSIZE"
        ]
    );
    assert_eq!(&fulltext.rows[4][22..], ["5", "1", "UNKNOWN", "0", "1"]);

    let callouts = [ReferenceCallout {
        label: Some("2"),
        first_author: Some("Figure"),
    }];
    let body = extract_fulltext_input(&doc, &all, &callouts);
    assert_eq!(body.token_indices, all);
    assert_eq!(&body.features.rows[4][24..], ["NUMBER", "1", "1"]);
    assert_eq!(body.select(&[4]).rows, &body.features.rows[4..5]);
    let caption_indices = &[2, 3];
    for caption in [
        extract_figure_features(&doc, caption_indices, &lexicon),
        extract_table_features(&doc, caption_indices, &lexicon),
    ] {
        assert_matrix(&caption, 27);
        assert_eq!(caption.rows, &fulltext.rows[2..4]);
    }

    let reference = extract_reference_segmenter_input(&doc, &all, &lexicon);
    assert_matrix(&reference.features, 28);
    assert_eq!(reference.line_indices, [0, 0, 1, 1, 2, 2]);
    assert_eq!(reference.token_indices, all);
    assert_row(
        &reference.features,
        0,
        "Figure figure F Fi Fig Figu e re ure gure LINESTART ALIGNEDLEFT INITCAP NODIGIT 0 0 0 0 0 0 0 0 NOPUNCT 0 8 BLOCKSTART no 0",
    );
    assert_eq!(
        extract_reference_segmenter_features(&doc, &all, &lexicon),
        reference.features
    );
    assert_matrix(
        &extract_name_citation_features_layout(&doc.tokens, &lexicon),
        21,
    );
}

#[test]
fn empty_and_whitespace_only_inputs() {
    let doc = LayoutDocument::default();
    let lexicon = Lexicon::empty();
    let tokens = ["\r\n", "\t", "\u{a0}", ""];
    for matrix in [
        extract_date_features(&tokens, &lexicon),
        extract_name_citation_features(&tokens, &lexicon),
        extract_name_header_features(&tokens, &[], &lexicon),
        extract_affiliation_features(&tokens, &[], &lexicon),
        extract_citation_features(&tokens, &[], &lexicon),
        extract_reference_segmenter_features_text(" \n\t", &lexicon),
        extract_reference_segmenter_features(&doc, &[2], &lexicon),
        extract_header_features(&doc, &[2], &lexicon),
        extract_fulltext_features(&doc, &[2], &lexicon),
        extract_figure_features(&doc, &[2], &lexicon),
        extract_table_features(&doc, &[2], &lexicon),
        extract_segmentation_features(&doc, &lexicon).0,
    ] {
        assert!(matrix.is_empty());
        assert_eq!(matrix.num_cols(), 0);
    }
}

#[test]
fn bundled_model_pattern_bounds() {
    let home = model_home();
    if !home.join("models").is_dir() {
        eprintln!("skipping Wapiti models in {}", home.display());
        return;
    }
    let doc = sample_layout();
    let all = [0, 1, 2, 3, 4, 5];
    let lexicon = Lexicon::empty();
    let tokens = ["A", "2008"];
    for (model, ntoks, features) in [
        ("date", 16, extract_date_features(&tokens, &lexicon)),
        (
            "name/citation",
            17,
            extract_name_citation_features(&tokens, &lexicon),
        ),
        (
            "name/header",
            19,
            extract_name_header_features(&tokens, &[], &lexicon),
        ),
        (
            "affiliation-address",
            20,
            extract_affiliation_features(&tokens, &[], &lexicon),
        ),
        (
            "citation",
            28,
            extract_citation_features(&tokens, &[], &lexicon),
        ),
        (
            "reference-segmenter",
            25,
            extract_reference_segmenter_features(&doc, &all, &lexicon),
        ),
        ("header", 28, extract_header_features(&doc, &all, &lexicon)),
        (
            "segmentation",
            32,
            extract_segmentation_features(&doc, &lexicon).0,
        ),
        (
            "fulltext",
            26,
            extract_fulltext_features(&doc, &all, &lexicon),
        ),
        ("figure", 25, extract_figure_features(&doc, &all, &lexicon)),
        ("table", 25, extract_table_features(&doc, &all, &lexicon)),
    ] {
        let file =
            std::fs::File::open(home.join("models").join(model).join("model.wapiti")).unwrap();
        let mut lines = BufReader::new(file).lines().map(Result::unwrap);
        assert!(lines.next().unwrap().starts_with("#mdl#"));
        let reader = lines.next().unwrap();
        let reader = reader
            .strip_prefix("#rdr#")
            .unwrap()
            .split('/')
            .collect::<Vec<_>>();
        assert_eq!(reader[1].parse::<usize>().unwrap(), ntoks, "{model}");
        let max_column = lines
            .take(reader[0].parse().unwrap())
            .flat_map(|pattern| {
                pattern
                    .split("%x[")
                    .skip(1)
                    .map(|part| {
                        part.split_once(',')
                            .unwrap()
                            .1
                            .split_once(']')
                            .unwrap()
                            .0
                            .parse::<usize>()
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .max()
            .unwrap();
        assert_eq!(max_column, ntoks, "{model}");
        assert!(features.num_cols() > max_column, "{model}");
        assert_model_vocabulary(model, &features);
    }
}

#[test]
fn real_wang_pdf() {
    let fixtures = std::env::var_os("NOSCO_PDF_FIXTURES")
        .or_else(|| std::env::var_os("GROBID_PDF_FIXTURES"))
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("NOSCO_GROBID_REF")
                .map(|r| PathBuf::from(r).join("grobid-core/src/test/resources"))
        })
        .unwrap_or_default();
    let path = fixtures.join("test/Wang-paperAVE2008.pdf");
    if !path.is_file() {
        eprintln!("skipping missing fixture {}", path.display());
        return;
    }
    let doc = nosco_pdf::extract_from_file(&path).unwrap();
    let home = model_home();
    let lexicon = if home.join("lexicon").is_dir() {
        Lexicon::load_from_grobid_home(&home).unwrap()
    } else {
        Lexicon::empty()
    };
    if home.join("lexicon").is_dir() {
        assert!(factory::is_proper_name("Mary", &lexicon));
        let names = extract_name_citation_features(
            &["Professor", " ", "Emeritus", " ", "Mary", " ", "Smith"],
            &lexicon,
        );
        assert_eq!(names.rows[0][17], "1");
        assert_eq!(names.rows[1][17], "1");
        assert_eq!(names.rows[2][15], "1");
        assert_eq!(names.rows[3][16], "1");
        let affiliation = extract_affiliation_features(&["South", "Africa"], &[], &lexicon);
        assert!(affiliation.rows.iter().all(|row| row[18] == "1"));
    }
    let (segmentation, line_indices) = extract_segmentation_features(&doc, &lexicon);
    assert_matrix(&segmentation, 33);
    assert_model_vocabulary("segmentation", &segmentation);
    assert_eq!(line_indices.len(), segmentation.len());
    for (line_index, row) in line_indices.into_iter().zip(segmentation.as_rows()) {
        assert!(
            doc.line_tokens(line_index)
                .first()
                .unwrap()
                .text
                .starts_with(&row[0])
                || row[0].starts_with(&doc.line_tokens(line_index).first().unwrap().text)
        );
    }
    let all: Vec<_> = (0..doc.tokens.len()).collect();
    let header = extract_header_features(&doc, &all[..50], &lexicon);
    assert_matrix(&header, 32);
    assert_model_vocabulary("header", &header);
    let raw_header = home.parent().unwrap_or(&home).join(
        "grobid-trainer/resources/dataset/header/corpus/raw/Wang-paperAVE2008.training.header",
    );
    if raw_header.is_file() {
        let upstream = std::fs::read_to_string(raw_header).unwrap();
        for (row, oracle) in header.rows.iter().zip(upstream.lines()).take(5) {
            assert_eq!(
                &row[..20],
                &oracle.split_whitespace().collect::<Vec<_>>()[..20]
            );
        }
    }
    assert_eq!(
        &header.rows[0][..10],
        [
            "Information",
            "information",
            "I",
            "In",
            "Inf",
            "Info",
            "n",
            "on",
            "ion",
            "tion"
        ]
    );
    let fulltext = extract_fulltext_features(&doc, &all, &lexicon);
    assert_matrix(&fulltext, 27);
    assert_model_vocabulary("fulltext", &fulltext);
    assert_eq!(fulltext.len(), doc.tokens.len());

    let references_start = doc
        .tokens
        .iter()
        .rposition(|token| token.text == "References")
        .unwrap();
    let reference = extract_reference_segmenter_input(&doc, &all[references_start..], &lexicon);
    assert_matrix(&reference.features, 28);
    assert_model_vocabulary("reference-segmenter", &reference.features);
    assert_eq!(reference.features.rows[0][0], "References");
    for ((row, token_index), line_index) in reference
        .features
        .as_rows()
        .iter()
        .zip(reference.token_indices)
        .zip(reference.line_indices)
    {
        assert_eq!(doc.tokens[token_index].text, row[0]);
        assert!(doc.lines[line_index].token_range.contains(&token_index));
    }
    for (word, extractor) in [
        (
            "fig",
            extract_figure_features as fn(&LayoutDocument, &[usize], &Lexicon) -> FeatureMatrix,
        ),
        ("table", extract_table_features),
    ] {
        let caption = doc
            .tokens
            .iter()
            .position(|token| {
                token.text.eq_ignore_ascii_case(word)
                    || token.text.to_ascii_lowercase().starts_with(word)
            })
            .unwrap_or_else(|| panic!("missing caption lead {word}"));
        let indices = doc.lines[doc.tokens[caption].line_idx]
            .token_range
            .clone()
            .collect::<Vec<_>>();
        let features = extractor(&doc, &indices, &lexicon);
        assert_matrix(&features, 27);
        assert_eq!(
            features.rows,
            fulltext.rows[indices[0]..indices[indices.len() - 1] + 1]
        );
    }
}
