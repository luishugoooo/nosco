use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use nosco_pdf::{LayoutDocument, extract_from_file};

fn collect_pdfs(directory: &Path, paths: &mut Vec<PathBuf>) {
    for entry in directory.read_dir().unwrap().map(Result::unwrap) {
        let path = entry.path();
        if path.is_dir() {
            collect_pdfs(&path, paths);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
        {
            paths.push(path);
        }
    }
}

#[test]
fn scholarly_pdfs() {
    let root = std::env::var_os("NOSCO_PDF_FIXTURES")
        .or_else(|| std::env::var_os("GROBID_PDF_FIXTURES"))
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("NOSCO_GROBID_REF")
                .map(|r| PathBuf::from(r).join("grobid-core/src/test/resources"))
        })
        .unwrap_or_default();
    if !root.join("test").is_dir() {
        eprintln!("skipping scholarly PDF fixtures in {}", root.display());
        return;
    }
    let mut paths = Vec::new();
    collect_pdfs(&root.join("test"), &mut paths);
    assert!(!paths.is_empty(), "no PDF files in {}", root.display());
    paths.extend(
        [
            "sample1/sample.pdf",
            "sample2/sample.pdf",
            "org/grobid/core/utilities/dehypenisation1.pdf",
        ]
        .iter()
        .map(|path| root.join(path))
        .filter(|path| path.is_file()),
    );
    paths.sort();

    for path in paths {
        let started = Instant::now();
        let doc =
            extract_from_file(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let text = doc.full_text();
        let count = text.chars().count();
        let replacement = text.chars().filter(|&ch| ch == '\u{fffd}').count();
        if path.ends_with("dehypenisation1.pdf") {
            assert_eq!(text, "We test the dehypenisa-\ntion of a short text.");
        } else {
            assert!(
                count >= 100
                    && text
                        .split_whitespace()
                        .filter(|word| word.chars().any(char::is_alphabetic))
                        .count()
                        >= 20,
                "{}: too little readable text",
                path.display(),
            );
        }
        match path.file_name().and_then(|name| name.to_str()) {
            Some("Wang-paperAVE2008.pdf") => {
                assert!(text.contains("Information Synthesis for Answer Validation"));
                assert!(text.contains("Günter Neumann"));
            }
            Some("test_Grobid_2_05982691.pdf") => {
                let page = doc.page_text(0);
                let position = |needle: &str| {
                    page.find(needle)
                        .unwrap_or_else(|| panic!("missing {needle}"))
                };
                assert!(position("Faisal Bashir Hussain") < position("Abstract"));
                assert!(position("Abstract") < position("I. INTRODUCTION"));
                assert!(
                    position("I. INTRODUCTION") < position("Mobility brings many advance concepts")
                );
                assert!(
                    position("Mobility brings many advance concepts")
                        < position("perception entirely shifting")
                );
                assert!(position("perception entirely shifting") < position("II. RELATED WORK"));
            }
            Some("sample.pdf") if path.ends_with("sample1/sample.pdf") => {
                assert!(text.contains("Bi-criteria Algorithm for Scheduling Jobs on Cluster"));
                assert!(text.contains("Pierre-François Dutot"));
                assert!(text.contains("Grégory Mounié"));
            }
            _ => {}
        }
        assert!(
            replacement * 25 < count,
            "{}: {replacement} replacement characters in {count} characters",
            path.display(),
        );
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "{}: {:?} to extract",
            path.display(),
            started.elapsed()
        );
        verify_indices(&doc);
        eprintln!(
            "{}: {} pages, {count} chars ({replacement} unmapped), {:?}",
            path.file_name().unwrap().to_string_lossy(),
            doc.pages.len(),
            started.elapsed()
        );
    }
}

fn verify_indices(doc: &LayoutDocument) {
    let mut next = (0, 0, 0);
    for (page_index, page) in doc.pages.iter().enumerate() {
        assert_eq!(page.number, page_index + 1);
        assert_eq!(
            (
                page.block_range.start,
                page.line_range.start,
                page.token_range.start
            ),
            next
        );
        assert!(page.width > 0.0 && page.height > 0.0);
        assert_eq!(page.main_area.page, page.number);
        for idx in page.block_range.clone() {
            let block = &doc.blocks[idx];
            assert_eq!(block.block_idx, idx);
            assert_eq!(block.page, page.number);
            assert!(!doc.block_tokens(idx).is_empty());
            assert!(
                page.token_range.start <= block.token_range.start
                    && block.token_range.end <= page.token_range.end
            );
            assert!(
                page.line_range.start <= block.line_range.start
                    && block.line_range.end <= page.line_range.end
            );
            let mut token_start = block.token_range.start;
            for line_idx in block.line_range.clone() {
                let line = &doc.lines[line_idx];
                assert_eq!(line.page, page.number);
                assert_eq!(line.block_idx, idx);
                assert_eq!(line.token_range.start, token_start);
                let line_tokens = doc.line_tokens(line_idx);
                assert!(line_tokens.last().is_some_and(|last| last.new_line_after));
                for token in line_tokens {
                    assert_eq!(token.page, page.number);
                    assert_eq!(token.block_idx, idx);
                    assert_eq!(token.line_idx, line_idx);
                    assert!(token.bbox.x >= 0.0 && token.bbox.x2() <= page.width + 0.001);
                    assert!(token.bbox.y >= 0.0 && token.bbox.y2() <= page.height + 0.001);
                }
                token_start = line.token_range.end;
            }
            assert_eq!(token_start, block.token_range.end);
        }
        next = (
            page.block_range.end,
            page.line_range.end,
            page.token_range.end,
        );
    }
    assert_eq!(next, (doc.blocks.len(), doc.lines.len(), doc.tokens.len()));
}
