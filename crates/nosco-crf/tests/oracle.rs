use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use nosco_crf::{CrfModel, DecodeResult, DecodeScratch};

fn reference() -> PathBuf {
    env::var_os("NOSCO_CRF_REFERENCE")
        .or_else(|| env::var_os("GROBID_CRF_REFERENCE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn model_path(reference: &Path, name: &str) -> PathBuf {
    reference
        .join("grobid-ref/grobid-home/models")
        .join(name)
        .join("model.wapiti")
}

#[test]
fn nosco_models_match_wapiti_oracle() {
    let reference = reference();
    let oracle = reference.join("wapiti-ref/wapiti");
    if !oracle.is_file() {
        eprintln!(
            "skipping Wapiti differential test: missing {}",
            oracle.display()
        );
        return;
    }
    let cases: &[(&str, &[&str])] = &[
        (
            "date",
            &["12 January 2020", "July 4 , 1776", "2018 - 05 - 14", "1968"],
        ),
        (
            "figure",
            &[
                "Figure 1 . Flow of the experiment",
                "Fig . 2 — Results at 25 °C",
                "FIGURE 3",
            ],
        ),
        (
            "name/citation",
            &[
                "Antti Airola , Sampo Pyysalo",
                "J . R . Smith Jr .",
                "Jean Dupont and Maria García",
            ],
        ),
        (
            "citation",
            &[
                "Proc . ESA Annual Meeting , 1999 , pp . 1 - 10 .",
                "Smith J . Learning under uncertainty . Nature 12 ( 2020 ) : 45 - 68 .",
                "Klein , M . ( 2013 ) . A practical guide . Berlin : Springer .",
            ],
        ),
    ];
    for &(name, sequences) in cases {
        let path = model_path(&reference, name);
        if !path.is_file() {
            eprintln!("skipping {name}: missing {}", path.display());
            continue;
        }
        let model = CrfModel::load_wapiti(&path).unwrap();
        let mut compact = Vec::new();
        model.write_gcrf(&mut compact).unwrap();
        let text_size = fs::metadata(&path).unwrap().len();
        assert!(
            (compact.len() as u64) * 3 < text_size,
            "{name} compact size {}",
            compact.len()
        );
        eprintln!(
            "{name}: {text_size} text bytes, {} GCRF bytes",
            compact.len()
        );
        let binary = CrfModel::from_gcrf_bytes(&compact).unwrap();
        assert_eq!(binary.stats(), model.stats());

        let inputs: Vec<_> = sequences
            .iter()
            .map(|seq| feature_rows(name, seq))
            .collect();
        let mut scratch = DecodeScratch::new();
        for posterior in [false, true] {
            let oracle_results = run_oracle(&oracle, &path, &inputs, posterior);
            assert_eq!(
                oracle_results.len(),
                inputs.len(),
                "{name}, posterior={posterior}"
            );
            for (input, answers) in inputs.iter().zip(oracle_results) {
                let cols: Vec<Vec<_>> = input
                    .iter()
                    .map(|r| r.split_ascii_whitespace().collect())
                    .collect();
                let rows: Vec<_> = cols.iter().map(Vec::as_slice).collect();
                let (result, roundtrip) = if posterior {
                    (
                        model.tag_posterior(&rows, &mut scratch).unwrap(),
                        binary.tag_posterior(&rows, &mut scratch).unwrap(),
                    )
                } else {
                    (
                        model.tag_with_scores(&rows, &mut scratch).unwrap(),
                        binary.tag_with_scores(&rows, &mut scratch).unwrap(),
                    )
                };
                assert_eq!(roundtrip, result);
                assert_scores(
                    &model,
                    &result,
                    answers,
                    &format!("{name}, posterior={posterior}, {input:?}"),
                );
            }
        }
    }
}

#[test]
fn segmentation_discards_millions_of_zero_observations() {
    let path = model_path(&reference(), "segmentation");
    if !path.is_file() {
        eprintln!(
            "skipping segmentation compaction test: missing {}",
            path.display()
        );
        return;
    }
    let model = CrfModel::load_wapiti(&path).unwrap();
    let stats = model.stats();
    assert_eq!(stats.num_unigram_weights + stats.num_bigram_weights, 18_244);
    assert!(stats.num_observations <= 18_244);
    let mut compact = Vec::new();
    model.write_gcrf(&mut compact).unwrap();
    let text_size = fs::metadata(path).unwrap().len();
    eprintln!(
        "segmentation: {text_size} text bytes, {} GCRF bytes",
        compact.len()
    );
    assert!((compact.len() as u64) * 50 < text_size);
}

struct OracleResult {
    sequence_score: f64,
    tokens: Vec<(String, f64)>,
}

fn run_oracle(
    oracle: &Path,
    model: &Path,
    inputs: &[Vec<String>],
    posterior: bool,
) -> Vec<OracleResult> {
    let mut cmd = Command::new(oracle);
    cmd.env("LC_ALL", "C").args(["label", "-l", "-s"]);
    if posterior {
        cmd.arg("-p");
    }
    let mut child = cmd
        .arg("-m")
        .arg(model)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for rows in inputs {
            for row in rows {
                writeln!(stdin, "{row}").unwrap();
            }
            writeln!(stdin).unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim_end()
        .split("\n\n")
        .map(|sequence| {
            let mut lines = sequence.lines();
            OracleResult {
                sequence_score: lines
                    .next()
                    .unwrap()
                    .strip_prefix("# 0 ")
                    .unwrap()
                    .parse()
                    .unwrap(),
                tokens: lines
                    .map(|line| {
                        let (label, score) = line.split_once('\t').unwrap();
                        let (scored_label, score) = score.rsplit_once('/').unwrap();
                        assert_eq!(label, scored_label);
                        (label.to_owned(), score.parse().unwrap())
                    })
                    .collect(),
            }
        })
        .collect()
}

fn assert_scores(model: &CrfModel, result: &DecodeResult, oracle: OracleResult, context: &str) {
    assert_eq!(result.label_ids.len(), oracle.tokens.len(), "{context}");
    assert!(
        (result.sequence_score - oracle.sequence_score).abs() <= 1e-6,
        "{context}: sequence scores {} and {}",
        result.sequence_score,
        oracle.sequence_score
    );
    for (at, (label, score)) in oracle.tokens.iter().enumerate() {
        assert_eq!(
            model.label_name(result.label_ids[at]),
            label,
            "{context}, token {at}"
        );
        assert!(
            (result.token_scores[at] - score).abs() <= 1e-6,
            "{context}, token {at}: scores {} and {score}",
            result.token_scores[at]
        );
    }
}

fn feature_rows(model: &str, sequence: &str) -> Vec<String> {
    let tokens: Vec<_> = sequence.split_ascii_whitespace().collect();
    tokens
        .iter()
        .enumerate()
        .map(|(i, &token)| {
            let chars: Vec<_> = token.chars().collect();
            let mut row = vec![token.to_owned(), token.to_lowercase()];
            for n in 1..=4 {
                row.push(chars.iter().take(n).collect());
            }
            for n in 1..=4 {
                row.push(chars[chars.len().saturating_sub(n)..].iter().collect());
            }
            let line = if i == 0 {
                "LINESTART"
            } else if i + 1 == tokens.len() {
                "LINEEND"
            } else {
                "LINEIN"
            };
            let digit = if token.bytes().all(|b| b.is_ascii_digit()) {
                "ALLDIGIT"
            } else {
                "NODIGIT"
            };
            let caps = if digit == "ALLDIGIT" {
                "NOCAPS"
            } else if token.bytes().any(|b| b.is_ascii_uppercase()) && token == token.to_uppercase()
            {
                "ALLCAP"
            } else if chars[0].is_uppercase() {
                "INITCAP"
            } else {
                "NOCAPS"
            };
            let single = if chars.len() == 1 { "1" } else { "0" };
            let year = if digit == "ALLDIGIT" && chars.len() == 4 {
                "1"
            } else {
                "0"
            };
            let month = if matches!(token, "January" | "July") {
                "1"
            } else {
                "0"
            };
            let punct = match token {
                "." => "DOT",
                "," => "COMMA",
                "-" => "HYPHEN",
                _ => "NOPUNCT",
            };
            let mut push = |cols: &[&str]| row.extend(cols.iter().map(|&s| s.to_owned()));
            match model {
                "date" => push(&[line, caps, digit, single, year, month, punct]),
                "name/citation" => {
                    push(&[line, caps, digit, single, "0", "0", "0", "0", "0", punct])
                }
                "figure" => push(&[
                    "BLOCKIN",
                    line,
                    "PAGEIN",
                    "SAMEFONT",
                    "SAMEFONTSIZE",
                    "0",
                    "0",
                    caps,
                    digit,
                    single,
                    "0",
                    "0",
                    "0",
                    year,
                    month,
                    "0",
                    "0",
                    punct,
                    "5",
                    "5",
                    "0",
                    "0",
                ]),
                "citation" => {
                    let position = (i * 12 / tokens.len()).to_string();
                    push(&[
                        line, caps, digit, single, "0", "0", "0", "0", "0", year, month, "0", "0",
                        "0", "0", "0", "0", punct, &position,
                    ]);
                }
                _ => unreachable!(),
            };
            row.join(" ")
        })
        .collect()
}
