use std::collections::HashMap;
use std::sync::OnceLock;

use nosco_models::{BibItem, BodyElement, Callout, CalloutKind, Document, Formula, Section};
use regex::Regex;

pub(crate) fn resolve(doc: &mut Document) {
    let ref_by_label: HashMap<String, String> = doc
        .references
        .iter()
        .filter_map(|bib| {
            let id = bib.id.clone()?;
            let label = bib.label.as_deref().map(normalize_label)?;
            (!label.is_empty()).then_some((label, id))
        })
        .collect();

    let fig_by_label: HashMap<String, String> = doc
        .figures
        .iter()
        .filter_map(|fig| {
            let id = fig.id.clone()?;
            let label = fig
                .label
                .as_deref()
                .and_then(extract_number_or_roman)
                .or_else(|| fig.header.as_deref().and_then(extract_number_or_roman))?;
            Some((label, id))
        })
        .collect();

    let tab_by_label: HashMap<String, String> = doc
        .tables
        .iter()
        .filter_map(|tab| {
            let id = tab.id.clone()?;
            let label = tab
                .label
                .as_deref()
                .and_then(extract_number_or_roman)
                .or_else(|| tab.header.as_deref().and_then(extract_number_or_roman))?;
            Some((label, id))
        })
        .collect();

    let mut formula_by_label = HashMap::new();
    collect_formulas(&doc.body, &mut formula_by_label);
    collect_formulas(&doc.annexes, &mut formula_by_label);

    let footnote_by_label: HashMap<String, String> = doc
        .footnotes
        .iter()
        .filter_map(|note| {
            let id = note.id.clone()?;
            let label = note.label.as_deref().map(normalize_label)?;
            (!label.is_empty()).then_some((label, id))
        })
        .collect();

    let ctx = ResolveCtx {
        references: &doc.references,
        ref_by_label,
        fig_by_label,
        fig_ids: doc.figures.iter().map(|f| f.id.clone()).collect(),
        tab_by_label,
        tab_ids: doc.tables.iter().map(|t| t.id.clone()).collect(),
        formula_by_label,
        footnote_by_label,
    };

    resolve_sections(&mut doc.body, &ctx);
    resolve_sections(&mut doc.annexes, &ctx);
}

struct ResolveCtx<'a> {
    references: &'a [BibItem],
    ref_by_label: HashMap<String, String>,
    fig_by_label: HashMap<String, String>,
    fig_ids: Vec<Option<String>>,
    tab_by_label: HashMap<String, String>,
    tab_ids: Vec<Option<String>>,
    formula_by_label: HashMap<String, String>,
    footnote_by_label: HashMap<String, String>,
}

fn collect_formulas(sections: &[Section], out: &mut HashMap<String, String>) {
    for section in sections {
        for elem in &section.elements {
            if let BodyElement::Formula(Formula {
                id: Some(id),
                label: Some(label),
                ..
            }) = elem
            {
                let norm = normalize_label(label);
                if !norm.is_empty() {
                    out.entry(norm.clone()).or_insert_with(|| id.clone());
                    let base = norm.trim_end_matches(|c: char| c.is_ascii_alphabetic());
                    if !base.is_empty() && base != norm {
                        out.entry(base.to_owned()).or_insert_with(|| id.clone());
                    }
                }
                if let Some(num) = extract_number_or_roman(label) {
                    out.entry(num).or_insert_with(|| id.clone());
                }
            }
        }
        collect_formulas(&section.subsections, out);
    }
}

fn is_numeric_citation_list(text: &str) -> bool {
    text.contains(',')
        && text.chars().any(|c| c.is_ascii_digit())
        && text.chars().all(|c| {
            c.is_ascii_digit()
                || c.is_whitespace()
                || matches!(c, ',' | '-' | '–' | '—' | '−' | '[' | ']' | '(' | ')')
        })
}

fn split_delimited_callout(callout: &Callout, delim: char) -> Vec<Callout> {
    let mut parts = Vec::new();
    let base = callout.char_start;
    let mut cursor = 0;
    for segment in callout.text.split(delim) {
        let seg_start = cursor;
        cursor += segment.len() + delim.len_utf8();
        let trimmed = segment.trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | ';' | ',')
        });
        if trimmed.is_empty() {
            continue;
        }
        let inner_offset = segment.find(trimmed).unwrap_or(0);
        let start = base + seg_start + inner_offset;
        let end = start + trimmed.len();
        parts.push(Callout {
            kind: callout.kind,
            text: trimmed.to_owned(),
            target_id: None,
            char_start: start,
            char_end: end,
        });
    }
    parts
}

fn resolve_sections(sections: &mut [Section], ctx: &ResolveCtx<'_>) {
    for section in sections {
        for elem in &mut section.elements {
            if let BodyElement::Paragraph(paragraph) = elem {
                let mut expanded = Vec::with_capacity(paragraph.callouts.len());
                for mut callout in std::mem::take(&mut paragraph.callouts) {
                    // Reclassify author-year citations mis-tagged as Formula markers.
                    if callout.kind == CalloutKind::Formula
                        && extract_year(&callout.text).is_some()
                        && callout.text.chars().any(char::is_alphabetic)
                    {
                        callout.kind = CalloutKind::Bibr;
                    }
                    if callout.kind == CalloutKind::Bibr {
                        let delim = if callout.text.contains(';') {
                            Some(';')
                        } else if is_numeric_citation_list(&callout.text) {
                            Some(',')
                        } else {
                            None
                        };
                        if let Some(delim) = delim {
                            let sub = split_delimited_callout(&callout, delim);
                            if sub.len() > 1 {
                                for mut piece in sub {
                                    if piece.target_id.is_none() {
                                        piece.target_id = resolve_callout(&piece, ctx);
                                    }
                                    expanded.push(piece);
                                }
                                continue;
                            }
                        }
                    }
                    if callout.target_id.is_none() {
                        callout.target_id = resolve_callout(&callout, ctx);
                    }
                    expanded.push(callout);
                }
                paragraph.callouts = expanded;
            }
        }
        resolve_sections(&mut section.subsections, ctx);
    }
}

fn resolve_callout(callout: &Callout, ctx: &ResolveCtx<'_>) -> Option<String> {
    match callout.kind {
        CalloutKind::Bibr => resolve_bibr(&callout.text, ctx),
        CalloutKind::Figure => resolve_numbered(&callout.text, &ctx.fig_by_label, &ctx.fig_ids),
        CalloutKind::Table => resolve_numbered(&callout.text, &ctx.tab_by_label, &ctx.tab_ids),
        CalloutKind::Formula => {
            if extract_year(&callout.text).is_some()
                && callout.text.chars().any(char::is_alphabetic)
            {
                return None;
            }
            let norm = normalize_label(&callout.text);
            ctx.formula_by_label.get(&norm).cloned().or_else(|| {
                let num = extract_number_or_roman(&callout.text)?;
                ctx.formula_by_label.get(&num).cloned()
            })
        }
        CalloutKind::Footnote => {
            let norm = normalize_label(&callout.text);
            ctx.footnote_by_label.get(&norm).cloned()
        }
    }
}

fn resolve_numbered(
    text: &str,
    by_label: &HashMap<String, String>,
    ordered_ids: &[Option<String>],
) -> Option<String> {
    let key = extract_number_or_roman(text)?;
    if let Some(id) = by_label.get(&key) {
        return Some(id.clone());
    }
    if let Ok(idx) = key.parse::<usize>()
        && idx >= 1
        && idx <= ordered_ids.len()
    {
        return ordered_ids[idx - 1].clone();
    }
    None
}

fn resolve_bibr(text: &str, ctx: &ResolveCtx<'_>) -> Option<String> {
    let norm = normalize_label(text);
    if !norm.is_empty()
        && let Some(id) = ctx.ref_by_label.get(&norm)
    {
        return Some(id.clone());
    }

    // Numeric reference like [1], (12), [3, 4], [5-7].
    if let Some(first_num) = extract_first_bib_number(text) {
        let key = first_num.to_string();
        if let Some(id) = ctx.ref_by_label.get(&key) {
            return Some(id.clone());
        }
        if first_num >= 1 && first_num <= ctx.references.len() {
            return ctx.references[first_num - 1].id.clone();
        }
    }

    // Author-year matching: e.g. "(Smith et al., 2020a)" or "Wang (2008)".
    let year = extract_year(text);
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| w.len() >= 2)
        .map(fold_surname)
        .filter(|w| {
            !matches!(
                w.as_str(),
                "et" | "al" | "and" | "see" | "cf" | "eg" | "ie" | "for" | "the" | "in" | "of"
            )
        })
        .collect();

    if words.is_empty() && year.is_none() {
        return None;
    }

    let mut best: Option<(usize, &str)> = None;
    let mut tied = false;
    for bib in ctx.references {
        let Some(id) = bib.id.as_deref() else {
            continue;
        };
        let bib_year = bib.date.as_ref().and_then(|d| d.year);
        let year_match = year.is_some() && bib_year == year;
        if year.is_some() && bib_year.is_some() && !year_match {
            continue;
        }
        let first_author_match = bib
            .authors
            .first()
            .and_then(|a| a.last_name.as_deref().or(a.raw_name.as_deref()))
            .is_some_and(|last| surname_matches(last, &words));
        let any_author_match = first_author_match
            || (year_match
                && bib.authors.iter().skip(1).take(2).any(|a| {
                    a.last_name
                        .as_deref()
                        .or(a.raw_name.as_deref())
                        .is_some_and(|last| surname_matches(last, &words))
                }));
        if !any_author_match {
            continue;
        }
        let second_author_bonus = usize::from(
            first_author_match
                && bib.authors.get(1).and_then(|a| a.last_name.as_deref()).is_some_and(|last| {
                    surname_matches(last, &words)
                }),
        );
        let score = (if first_author_match { 3 } else { 1 })
            + 2 * usize::from(year_match)
            + second_author_bonus;
        match best {
            None => {
                best = Some((score, id));
                tied = false;
            }
            Some((best_score, _)) if score > best_score => {
                best = Some((score, id));
                tied = false;
            }
            Some((best_score, _)) if score == best_score => {
                tied = true;
            }
            _ => {}
        }
    }
    if !tied {
        best.map(|(_, id)| id.to_owned())
    } else {
        None
    }
}

fn surname_matches(raw_last: &str, callout_words: &[String]) -> bool {
    let folded = fold_surname(raw_last);
    if folded.len() >= 2 && callout_words.iter().any(|w| w == &folded) {
        return true;
    }
    raw_last
        .split(|c: char| !c.is_alphabetic())
        .filter(|part| part.len() >= 3)
        .map(fold_surname)
        .any(|part| callout_words.iter().any(|w| w == &part))
}

fn fold_surname(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphabetic())
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' => 'a',
            'é' | 'è' | 'ê' | 'ë' | 'ē' => 'e',
            'í' | 'ì' | 'î' | 'ï' | 'ī' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'ō' => 'o',
            'ú' | 'ù' | 'û' | 'ü' | 'ū' => 'u',
            'ñ' => 'n',
            'ç' | 'ć' | 'č' => 'c',
            'š' | 'ś' => 's',
            'ž' | 'ź' | 'ż' => 'z',
            'ł' => 'l',
            'ý' | 'ÿ' => 'y',
            _ => c,
        })
        .collect()
}

fn normalize_label(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c: char| matches!(c, '[' | ']' | '(' | ')' | '.' | ',' | ':' | ';' | ' '))
        .to_lowercase()
}

fn extract_first_bib_number(text: &str) -> Option<usize> {
    static NUM_RE: OnceLock<Regex> = OnceLock::new();
    let re = NUM_RE.get_or_init(|| Regex::new(r"\b(\d{1,4})\b").expect("valid regex"));
    let trimmed = text.trim();
    // Avoid mistaking a 4-digit publication year in an author-year callout for a ref index.
    let has_letters = trimmed.chars().any(|c| c.is_alphabetic());
    if has_letters {
        return None;
    }
    let cap = re.captures(trimmed)?;
    let n: usize = cap[1].parse().ok()?;
    if (1800..=2100).contains(&n) {
        return None;
    }
    Some(n)
}

fn extract_year(text: &str) -> Option<i32> {
    static YEAR_RE: OnceLock<Regex> = OnceLock::new();
    let re = YEAR_RE.get_or_init(|| Regex::new(r"\b((?:18|19|20)\d{2})[a-z]?\b").expect("valid regex"));
    re.captures(text)?.get(1)?.as_str().parse().ok()
}

fn extract_number_or_roman(text: &str) -> Option<String> {
    static LABEL_RE: OnceLock<Regex> = OnceLock::new();
    let re = LABEL_RE.get_or_init(|| {
        Regex::new(r"(?i)\b((?:s|e)?\d+(?:\.\d+)?)[a-z]?\b|\b([ivxlcdm]{1,5})\b")
            .expect("valid regex")
    });
    for cap in re.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            let s = m.as_str().to_lowercase();
            // Strip leading 'e' from "Extended Data" if only numeric lookup needed, keep 's' for S1.
            return Some(s);
        }
        if let Some(m) = cap.get(2) {
            let s = m.as_str().to_lowercase();
            if matches!(
                s.as_str(),
                "i" | "ii" | "iii" | "iv" | "v" | "vi" | "vii" | "viii" | "ix" | "x" | "xi" | "xii"
            ) {
                return Some(s);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use nosco_models::{Date, Figure, Paragraph, Person, Table};

    #[test]
    fn resolves_numeric_and_author_year_and_figure_table_callouts() {
        let mut doc = Document {
            references: vec![
                BibItem {
                    id: Some("b0".into()),
                    label: Some("[1]".into()),
                    authors: vec![Person {
                        last_name: Some("Wang".into()),
                        ..Default::default()
                    }],
                    date: Some(Date {
                        year: Some(2008),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                BibItem {
                    id: Some("b1".into()),
                    label: Some("[2]".into()),
                    authors: vec![Person {
                        last_name: Some("Smith".into()),
                        ..Default::default()
                    }],
                    date: Some(Date {
                        year: Some(2020),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            figures: vec![Figure {
                id: Some("fig_0".into()),
                label: Some("1".into()),
                ..Default::default()
            }],
            tables: vec![Table {
                id: Some("tab_0".into()),
                label: Some("II".into()),
                ..Default::default()
            }],
            body: vec![Section {
                elements: vec![
                    BodyElement::Paragraph(Paragraph {
                        text: "See [1], (Smith et al., 2020), Fig. 1, Table II, Eq. (3).".into(),
                        callouts: vec![
                            Callout {
                                kind: CalloutKind::Bibr,
                                text: "[1]".into(),
                                target_id: None,
                                char_start: 4,
                                char_end: 7,
                            },
                            Callout {
                                kind: CalloutKind::Bibr,
                                text: "(Smith et al., 2020)".into(),
                                target_id: None,
                                char_start: 9,
                                char_end: 29,
                            },
                            Callout {
                                kind: CalloutKind::Figure,
                                text: "Fig. 1".into(),
                                target_id: None,
                                char_start: 31,
                                char_end: 37,
                            },
                            Callout {
                                kind: CalloutKind::Table,
                                text: "Table II".into(),
                                target_id: None,
                                char_start: 39,
                                char_end: 47,
                            },
                            Callout {
                                kind: CalloutKind::Formula,
                                text: "Eq. (3)".into(),
                                target_id: None,
                                char_start: 49,
                                char_end: 56,
                            },
                        ],
                        bboxes: Vec::new(),
                    }),
                    BodyElement::Formula(Formula {
                        id: Some("formula_0".into()),
                        label: Some("(3)".into()),
                        text: "E = mc^2".into(),
                        bboxes: Vec::new(),
                    }),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };

        resolve(&mut doc);

        let BodyElement::Paragraph(p) = &doc.body[0].elements[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(p.callouts[0].target_id.as_deref(), Some("b0"));
        assert_eq!(p.callouts[1].target_id.as_deref(), Some("b1"));
        assert_eq!(p.callouts[2].target_id.as_deref(), Some("fig_0"));
        assert_eq!(p.callouts[3].target_id.as_deref(), Some("tab_0"));
        assert_eq!(p.callouts[4].target_id.as_deref(), Some("formula_0"));
    }

    #[test]
    fn splits_semicolon_citations_and_strips_subfigure_panels() {
        let mut doc = Document {
            references: vec![
                BibItem {
                    id: Some("b0".into()),
                    authors: vec![Person { last_name: Some("Müller".into()), ..Default::default() }],
                    date: Some(Date { year: Some(2019), ..Default::default() }),
                    ..Default::default()
                },
                BibItem {
                    id: Some("b1".into()),
                    authors: vec![Person { last_name: Some("Smith".into()), ..Default::default() }],
                    date: Some(Date { year: Some(2020), ..Default::default() }),
                    ..Default::default()
                },
            ],
            figures: vec![Figure {
                id: Some("fig_1".into()),
                label: Some("2".into()),
                ..Default::default()
            }],
            body: vec![Section {
                elements: vec![BodyElement::Paragraph(Paragraph {
                    text: "Prior work (Muller 2019; Smith et al., 2020a) and Fig. 2b.".into(),
                    callouts: vec![
                        Callout {
                            kind: CalloutKind::Bibr,
                            text: "(Muller 2019; Smith et al., 2020a)".into(),
                            target_id: None,
                            char_start: 11,
                            char_end: 45,
                        },
                        Callout {
                            kind: CalloutKind::Figure,
                            text: "Fig. 2b".into(),
                            target_id: None,
                            char_start: 50,
                            char_end: 57,
                        },
                    ],
                    bboxes: Vec::new(),
                })],
                ..Default::default()
            }],
            ..Default::default()
        };

        resolve(&mut doc);

        let BodyElement::Paragraph(p) = &doc.body[0].elements[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(p.callouts.len(), 3);
        assert_eq!(p.callouts[0].text, "Muller 2019");
        assert_eq!(p.callouts[0].target_id.as_deref(), Some("b0"));
        assert_eq!(p.callouts[1].text, "Smith et al., 2020a");
        assert_eq!(p.callouts[1].target_id.as_deref(), Some("b1"));
        assert_eq!(p.callouts[2].target_id.as_deref(), Some("fig_1"));
    }

    #[test]
    fn splits_numeric_lists_and_reclassifies_author_year_formulas() {
        let mut doc = Document {
            references: vec![
                BibItem {
                    id: Some("b0".into()),
                    authors: vec![Person { last_name: Some("Sutskever".into()), ..Default::default() }],
                    date: Some(Date { year: Some(2014), ..Default::default() }),
                    ..Default::default()
                },
                BibItem {
                    id: Some("b1".into()),
                    label: Some("2".into()),
                    ..Default::default()
                },
            ],
            body: vec![Section {
                elements: vec![BodyElement::Paragraph(Paragraph {
                    text: "See [1, 2] and Sutskever et al. (2014).".into(),
                    callouts: vec![
                        Callout {
                            kind: CalloutKind::Bibr,
                            text: "[1, 2]".into(),
                            target_id: None,
                            char_start: 4,
                            char_end: 10,
                        },
                        Callout {
                            kind: CalloutKind::Formula,
                            text: "Sutskever et al. (2014)".into(),
                            target_id: None,
                            char_start: 15,
                            char_end: 38,
                        },
                    ],
                    bboxes: Vec::new(),
                })],
                ..Default::default()
            }],
            ..Default::default()
        };

        resolve(&mut doc);

        let BodyElement::Paragraph(p) = &doc.body[0].elements[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(p.callouts.len(), 3);
        assert_eq!(p.callouts[0].target_id.as_deref(), Some("b0"));
        assert_eq!(p.callouts[1].target_id.as_deref(), Some("b1"));
        assert_eq!(p.callouts[2].kind, CalloutKind::Bibr);
        assert_eq!(p.callouts[2].target_id.as_deref(), Some("b0"));
    }
}
