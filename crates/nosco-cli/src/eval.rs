use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use nosco_crf::DecodeScratch;
use nosco_engine::{Engine, ModelKind};
use nosco_features::{
    extract_affiliation_features, extract_citation_features, extract_date_features,
    extract_name_citation_features, extract_name_header_features,
};
use nosco_lexicon::tokenizer;
use quick_xml::Reader;
use quick_xml::events::Event;

#[derive(Default, Clone, Debug)]
struct LabelCounts {
    gold: usize,
    pred: usize,
    correct: usize,
}

impl LabelCounts {
    fn precision(&self) -> f64 {
        if self.pred == 0 {
            0.0
        } else {
            self.correct as f64 / self.pred as f64
        }
    }

    fn recall(&self) -> f64 {
        if self.gold == 0 {
            0.0
        } else {
            self.correct as f64 / self.gold as f64
        }
    }

    fn f1(&self) -> f64 {
        let p = self.precision();
        let r = self.recall();
        if p + r == 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        }
    }
}

#[derive(Debug)]
struct LabeledSequence {
    tokens: Vec<String>,
    line_starts: Vec<bool>,
    gold_labels: Vec<String>,
}

pub fn run_eval(engine: &Engine, dataset_root: &Path, limit: Option<usize>) -> Result<()> {
    let tasks: &[(&str, ModelKind, &[&str])] = &[
        ("date", ModelKind::Date, &["date/corpus"]),
        (
            "name/citation",
            ModelKind::NameCitation,
            &["name/citation/evaluation", "name/citation/corpus"],
        ),
        (
            "name/header",
            ModelKind::NameHeader,
            &["name/header/evaluation", "name/header/corpus"],
        ),
        (
            "affiliation-address",
            ModelKind::AffiliationAddress,
            &["affiliation-address/corpus"],
        ),
        ("citation", ModelKind::Citation, &["citation/corpus"]),
    ];

    println!("=== nosco Token-Level Evaluation (vs. GROBID Gold XML Corpora) ===");
    for &(name, kind, rel_dirs) in tasks {
        let Some(dir) = rel_dirs
            .iter()
            .map(|r| dataset_root.join(r))
            .find(|p| p.is_dir())
        else {
            println!("{name}: skipped (directory not found under {})", dataset_root.display());
            continue;
        };

        let xml_files = collect_xml_files(&dir)?;
        let mut sequences = Vec::new();
        for file in xml_files {
            if let Some(max_seqs) = limit
                && sequences.len() >= max_seqs
            {
                break;
            }
            let Ok(content) = fs::read_to_string(&file) else {
                continue;
            };
            let remaining = limit.map(|m| m.saturating_sub(sequences.len()));
            extract_sequences(&content, kind, remaining, &mut sequences);
        }

        if sequences.is_empty() {
            println!("{name}: no sequences extracted from {}", dir.display());
            continue;
        }

        let model = engine.model(kind);
        let lexicon = engine.lexicon();
        let mut scratch = DecodeScratch::new();
        let mut per_label: BTreeMap<String, LabelCounts> = BTreeMap::new();
        let mut total_tokens = 0usize;
        let mut correct_tokens = 0usize;

        let t0 = Instant::now();
        for seq in &sequences {
            let tok_refs: Vec<&str> = seq.tokens.iter().map(String::as_str).collect();
            let features = match kind {
                ModelKind::Date => extract_date_features(&tok_refs, lexicon),
                ModelKind::NameCitation => extract_name_citation_features(&tok_refs, lexicon),
                ModelKind::NameHeader => {
                    extract_name_header_features(&tok_refs, &seq.line_starts, lexicon)
                }
                ModelKind::AffiliationAddress => {
                    extract_affiliation_features(&tok_refs, &seq.line_starts, lexicon)
                }
                ModelKind::Citation => {
                    extract_citation_features(&tok_refs, &seq.line_starts, lexicon)
                }
                _ => continue,
            };

            let pred_ids = model
                .tag(features.as_rows(), &mut scratch)
                .with_context(|| format!("tagging sequence for {name}"))?;

            for (pred_id, gold_tag) in pred_ids.into_iter().zip(&seq.gold_labels) {
                let pred_tag = normalize_eval_tag(model.label_name(pred_id));
                let gold_norm = normalize_eval_tag(gold_tag);
                total_tokens += 1;
                if pred_tag == gold_norm {
                    correct_tokens += 1;
                }
                if gold_norm != "<other>" {
                    per_label.entry(gold_norm.clone()).or_default().gold += 1;
                }
                if pred_tag != "<other>" {
                    per_label.entry(pred_tag.clone()).or_default().pred += 1;
                }
                if pred_tag == gold_norm && gold_norm != "<other>" {
                    per_label.entry(gold_norm).or_default().correct += 1;
                }
            }
        }
        let elapsed = t0.elapsed();
        let acc = if total_tokens == 0 {
            0.0
        } else {
            100.0 * correct_tokens as f64 / total_tokens as f64
        };
        let (sum_gold, sum_pred, sum_corr) = per_label.values().fold((0, 0, 0), |a, c| {
            (a.0 + c.gold, a.1 + c.pred, a.2 + c.correct)
        });
        let micro = LabelCounts {
            gold: sum_gold,
            pred: sum_pred,
            correct: sum_corr,
        };
        let seq_per_sec = sequences.len() as f64 / elapsed.as_secs_f64().max(1e-9);
        println!(
            "\n[{name}] seqs={} tokens={} | accuracy={acc:.2}% | micro-P={:.2}% micro-R={:.2}% micro-F1={:.2}% | {:.0} seq/s ({:.1} ms)",
            sequences.len(),
            total_tokens,
            micro.precision() * 100.0,
            micro.recall() * 100.0,
            micro.f1() * 100.0,
            seq_per_sec,
            elapsed.as_secs_f64() * 1000.0,
        );
        for (label, c) in &per_label {
            if c.gold >= 5 || c.pred >= 5 {
                println!(
                    "  {label:<18} P={:5.2}%  R={:5.2}%  F1={:5.2}%  (gold={}, pred={})",
                    c.precision() * 100.0,
                    c.recall() * 100.0,
                    c.f1() * 100.0,
                    c.gold,
                    c.pred
                );
            }
        }
    }

    if let Some(rss_mib) = peak_rss_mib() {
        println!("\nPeak RSS (VmHWM): {rss_mib:.1} MiB");
    }
    Ok(())
}

pub fn peak_rss_mib() -> Option<f64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kb: f64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb / 1024.0);
        }
    }
    None
}

fn normalize_eval_tag(raw: &str) -> String {
    raw.strip_prefix("I-").unwrap_or(raw).to_owned()
}

fn collect_xml_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("xml"))
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn extract_sequences(
    xml: &str,
    kind: ModelKind,
    limit: Option<usize>,
    out: &mut Vec<LabeledSequence>,
) {
    let container_tag = match kind {
        ModelKind::Date => "date",
        ModelKind::NameCitation | ModelKind::NameHeader => "author",
        ModelKind::AffiliationAddress => "affiliation",
        ModelKind::Citation => "bibl",
        _ => return,
    };

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = false;

    let mut in_container = false;
    let mut depth = 0usize;
    let mut current_tag = "<other>".to_owned();
    let mut tag_stack: Vec<String> = Vec::new();
    let mut tokens: Vec<String> = Vec::new();
    let mut line_starts: Vec<bool> = Vec::new();
    let mut gold_labels: Vec<String> = Vec::new();
    let mut pending_newline = true;
    let mut added = 0usize;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_owned();
                if !in_container && name == container_tag {
                    in_container = true;
                    depth = 1;
                    tokens.clear();
                    line_starts.clear();
                    gold_labels.clear();
                    tag_stack.clear();
                    current_tag = "<other>".to_owned();
                    pending_newline = true;
                } else if in_container {
                    depth += 1;
                    let mapped = map_xml_tag(kind, &name, &e);
                    tag_stack.push(current_tag.clone());
                    if let Some(m) = mapped {
                        current_tag = m.to_owned();
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                if in_container && e.name().as_ref() == "lb" {
                    pending_newline = true;
                }
            }
            Ok(Event::Text(e)) => {
                if in_container {
                    let raw: &str = e.as_ref();
                    let text = quick_xml::escape::unescape(raw)
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| raw.to_owned());
                    for piece in tokenizer::tokenize_with_spaces(&text) {
                        if piece.contains('\n') || piece.contains('\r') {
                            pending_newline = true;
                            continue;
                        }
                        if piece.chars().all(char::is_whitespace) {
                            continue;
                        }
                        tokens.push(piece);
                        line_starts.push(pending_newline);
                        gold_labels.push(current_tag.clone());
                        pending_newline = false;
                    }
                }
            }
            Ok(Event::End(e)) => {
                if in_container {
                    let qname = e.name();
                    let name = qname.as_ref();
                    depth = depth.saturating_sub(1);
                    if depth == 0 || name == container_tag {
                        if !tokens.is_empty() {
                            out.push(LabeledSequence {
                                tokens: std::mem::take(&mut tokens),
                                line_starts: std::mem::take(&mut line_starts),
                                gold_labels: std::mem::take(&mut gold_labels),
                            });
                            added += 1;
                            if limit.is_some_and(|m| added >= m) {
                                return;
                            }
                        }
                        in_container = false;
                        tag_stack.clear();
                        current_tag = "<other>".to_owned();
                    } else if let Some(prev) = tag_stack.pop() {
                        current_tag = prev;
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn attr_value(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .map(|a| a.value.into_owned())
}

fn map_xml_tag(
    kind: ModelKind,
    name: &str,
    e: &quick_xml::events::BytesStart<'_>,
) -> Option<&'static str> {
    match kind {
        ModelKind::Date => match name {
            "year" => Some("<year>"),
            "month" => Some("<month>"),
            "day" => Some("<day>"),
            _ => None,
        },
        ModelKind::NameCitation | ModelKind::NameHeader => match name {
            "forename" => Some("<forename>"),
            "middlename" => Some("<middlename>"),
            "surname" | "lastname" => Some("<surname>"),
            "roleName" | "title" => Some("<title>"),
            "genName" | "suffix" => Some("<suffix>"),
            "marker" => Some("<marker>"),
            _ => None,
        },
        ModelKind::AffiliationAddress => match name {
            "orgName" => match attr_value(e, "type").as_deref() {
                Some("department") => Some("<department>"),
                Some("laboratory") => Some("<laboratory>"),
                _ => Some("<institution>"),
            },
            "settlement" => Some("<settlement>"),
            "region" => Some("<region>"),
            "country" => Some("<country>"),
            "postCode" => Some("<postCode>"),
            "postBox" => Some("<postBox>"),
            "addrLine" => Some("<addrLine>"),
            "marker" => Some("<marker>"),
            _ => None,
        },
        ModelKind::Citation => match name {
            "author" => Some("<author>"),
            "editor" => Some("<editor>"),
            "title" => match attr_value(e, "level").as_deref() {
                Some("j") => Some("<journal>"),
                Some("m") => Some("<booktitle>"),
                Some("s") => Some("<series>"),
                _ => Some("<title>"),
            },
            "date" => Some("<date>"),
            "biblScope" => match attr_value(e, "unit")
                .or_else(|| attr_value(e, "type"))
                .as_deref()
            {
                Some("vol" | "volume") => Some("<volume>"),
                Some("issue") => Some("<issue>"),
                Some("pp" | "page") => Some("<pages>"),
                _ => None,
            },
            "publisher" => Some("<publisher>"),
            "pubPlace" => Some("<location>"),
            "orgName" => match attr_value(e, "type").as_deref() {
                Some("collaboration") => Some("<collaboration>"),
                _ => Some("<institution>"),
            },
            "idno" => Some("<pubnum>"),
            "ptr" => Some("<web>"),
            "note" => match attr_value(e, "type").as_deref() {
                Some("report_type") => Some("<tech>"),
                _ => Some("<note>"),
            },
            _ => None,
        },
        _ => None,
    }
}
