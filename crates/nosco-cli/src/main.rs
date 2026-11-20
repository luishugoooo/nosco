mod eval;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use nosco_engine::{Engine, convert_grobid_home};
use nosco_models::{TeiOptions, header_to_tei_xml, references_to_tei_xml};
use rayon::prelude::*;

#[derive(Parser)]
#[command(
    name = "nosco",
    version,
    about = "Fast, pure-Rust scholarly PDF and bibliographic extraction engine"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ProcessMode {
    Fulltext,
    Header,
    References,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum OutputFormat {
    Json,
    Tei,
}

#[derive(Subcommand)]
enum Command {
    /// Convert an upstream GROBID `grobid-home` into a compact binary `.gcrf`/`.glex` asset pack.
    Setup {
        /// Path to upstream GROBID `grobid-home` directory (containing `models/` and `lexicon/`).
        #[arg(long)]
        grobid_home: PathBuf,
        /// Destination directory for compact `.gcrf` models and `lexicon.glex`.
        #[arg(long, short)]
        out: PathBuf,
    },
    /// Process a PDF file or directory of PDFs into JSON or TEI XML.
    Process {
        /// Input PDF file or directory containing PDFs.
        input: PathBuf,
        /// Path to compact asset pack directory (or raw `grobid-home`).
        #[arg(long, short)]
        models: PathBuf,
        /// Extraction mode.
        #[arg(long, value_enum, default_value_t = ProcessMode::Fulltext)]
        mode: ProcessMode,
        /// Output format.
        #[arg(long, short, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        /// Pretty-print JSON or TEI XML output.
        #[arg(long)]
        pretty: bool,
        /// Include PDF coordinates in TEI XML output.
        #[arg(long)]
        coords: bool,
        /// Include raw citation strings in TEI XML output.
        #[arg(long)]
        raw_citations: bool,
        /// Output file (for single PDF) or output directory (for batch directory).
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Number of parallel worker threads (defaults to Rayon default).
        #[arg(long, short)]
        jobs: Option<usize>,
    },
    /// Parse a raw bibliographic citation string.
    ParseCitation {
        /// Raw citation string.
        #[arg(required = true)]
        text: Vec<String>,
        #[arg(long, short)]
        models: PathBuf,
        #[arg(long, short, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[arg(long)]
        pretty: bool,
    },
    /// Parse an author/name string.
    ParseNames {
        #[arg(required = true)]
        text: Vec<String>,
        #[arg(long, short)]
        models: PathBuf,
        /// Use citation-style name model instead of header-style name model.
        #[arg(long)]
        citation: bool,
        #[arg(long)]
        pretty: bool,
    },
    /// Parse an affiliation/address string.
    ParseAffiliation {
        #[arg(required = true)]
        text: Vec<String>,
        #[arg(long, short)]
        models: PathBuf,
        #[arg(long)]
        pretty: bool,
    },
    /// Parse a date string.
    ParseDate {
        #[arg(required = true)]
        text: Vec<String>,
        #[arg(long, short)]
        models: PathBuf,
        #[arg(long)]
        pretty: bool,
    },
    /// Evaluate token-level precision/recall/F1 on GROBID gold XML corpora.
    Eval {
        #[arg(long, short)]
        models: PathBuf,
        /// Path to `grobid-trainer/resources/dataset`.
        #[arg(long, short)]
        dataset: PathBuf,
        /// Optional maximum sequences per model.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Benchmark startup time, throughput, latency, and peak RSS on a PDF file or directory.
    Bench {
        /// Input PDF file or directory.
        input: PathBuf,
        #[arg(long, short)]
        models: PathBuf,
        #[arg(long, value_enum, default_value_t = ProcessMode::Fulltext)]
        mode: ProcessMode,
        #[arg(long, short)]
        jobs: Option<usize>,
        #[arg(long, default_value_t = 1)]
        iterations: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Setup { grobid_home, out } => cmd_setup(&grobid_home, &out),
        Command::Process {
            input,
            models,
            mode,
            format,
            pretty,
            coords,
            raw_citations,
            output,
            jobs,
        } => cmd_process(
            &input,
            &models,
            mode,
            format,
            pretty,
            coords,
            raw_citations,
            output.as_deref(),
            jobs,
        ),
        Command::ParseCitation {
            text,
            models,
            format,
            pretty,
        } => {
            let engine = Engine::load(&models)?;
            let joined = text.join(" ");
            let bib = engine.parse_citation(&joined)?;
            let out = match format {
                OutputFormat::Json => {
                    if pretty {
                        serde_json::to_string_pretty(&bib)?
                    } else {
                        serde_json::to_string(&bib)?
                    }
                }
                OutputFormat::Tei => bib.to_tei_xml(&TeiOptions {
                    pretty,
                    include_coordinates: false,
                    include_raw_citations: true,
                })?,
            };
            println!("{out}");
            Ok(())
        }
        Command::ParseNames {
            text,
            models,
            citation,
            pretty,
        } => {
            let engine = Engine::load(&models)?;
            let joined = text.join(" ");
            let names = if citation {
                engine.parse_citation_names(&joined)?
            } else {
                engine.parse_header_names(&joined)?
            };
            print_json(&names, pretty)
        }
        Command::ParseAffiliation {
            text,
            models,
            pretty,
        } => {
            let engine = Engine::load(&models)?;
            let affs = engine.parse_affiliations(&text.join(" "))?;
            print_json(&affs, pretty)
        }
        Command::ParseDate {
            text,
            models,
            pretty,
        } => {
            let engine = Engine::load(&models)?;
            let dates = engine.parse_date(&text.join(" "))?;
            print_json(&dates, pretty)
        }
        Command::Eval {
            models,
            dataset,
            limit,
        } => {
            let t0 = Instant::now();
            let engine = Engine::load(&models)?;
            println!(
                "Loaded engine in {:.1} ms",
                t0.elapsed().as_secs_f64() * 1000.0
            );
            eval::run_eval(&engine, &dataset, limit)
        }
        Command::Bench {
            input,
            models,
            mode,
            jobs,
            iterations,
        } => cmd_bench(&input, &models, mode, jobs, iterations),
    }
}

fn print_json<T: serde::Serialize>(value: &T, pretty: bool) -> Result<()> {
    let s = if pretty {
        serde_json::to_string_pretty(value)?
    } else {
        serde_json::to_string(value)?
    };
    println!("{s}");
    Ok(())
}

fn configure_threads(jobs: Option<usize>) -> Result<()> {
    if let Some(n) = jobs {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n.max(1))
            .build_global()
            .ok();
    }
    Ok(())
}

fn cmd_setup(grobid_home: &Path, out: &Path) -> Result<()> {
    let t0 = Instant::now();
    let report = convert_grobid_home(grobid_home, out)?;
    let elapsed = t0.elapsed();

    println!(
        "{:<22} {:>10} {:>10} {:>8}",
        "Model", "Wapiti", "GCRF", "Ratio"
    );
    println!("{}", "-".repeat(54));
    let mut total_model_out = 0u64;
    for stat in &report.models {
        total_model_out += stat.output_bytes;
        let ratio = stat.input_bytes as f64 / stat.output_bytes.max(1) as f64;
        println!(
            "{:<22} {:>10} {:>10} {:>7.1}x",
            stat.kind.to_string(),
            fmt_bytes(stat.input_bytes),
            fmt_bytes(stat.output_bytes),
            ratio,
        );
    }
    println!("{}", "-".repeat(54));
    println!(
        "{:<22} {:>10} {:>10} {:>7.1}x",
        "All 11 CRF models",
        fmt_bytes(report.total_input_bytes),
        fmt_bytes(total_model_out),
        report.total_input_bytes as f64 / total_model_out.max(1) as f64,
    );
    println!(
        "{:<22} {:>10} {:>10}",
        "Lexicon (.glex)",
        "-",
        fmt_bytes(report.lexicon_output_bytes),
    );
    println!(
        "Total asset pack: {} (written to {} in {:.2} s)",
        fmt_bytes(report.total_output_bytes),
        out.display(),
        elapsed.as_secs_f64()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_process(
    input: &Path,
    models: &Path,
    mode: ProcessMode,
    format: OutputFormat,
    pretty: bool,
    coords: bool,
    raw_citations: bool,
    output: Option<&Path>,
    jobs: Option<usize>,
) -> Result<()> {
    configure_threads(jobs)?;
    let engine = Engine::load(models)?;
    let tei_opts = TeiOptions {
        pretty,
        include_coordinates: coords,
        include_raw_citations: raw_citations,
    };

    if input.is_file() {
        let rendered = render_single_pdf(&engine, input, mode, format, pretty, &tei_opts)?;
        if let Some(out_path) = output {
            if let Some(parent) = out_path.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent)?;
            }
            fs::write(out_path, rendered)
                .with_context(|| format!("writing {}", out_path.display()))?;
        } else {
            println!("{rendered}");
        }
        return Ok(());
    }

    if !input.is_dir() {
        bail!("input path does not exist: {}", input.display());
    }

    let pdfs = collect_pdfs(input)?;
    if pdfs.is_empty() {
        bail!("no PDF files found under {}", input.display());
    }
    let out_dir = output.unwrap_or(input);
    fs::create_dir_all(out_dir)?;
    let ext = match format {
        OutputFormat::Json => "json",
        OutputFormat::Tei => "tei.xml",
    };

    let t0 = Instant::now();
    let results: Vec<Result<()>> = pdfs
        .par_iter()
        .map(|pdf| {
            let rendered = render_single_pdf(&engine, pdf, mode, format, pretty, &tei_opts)?;
            let stem = pdf
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("document");
            let dest = out_dir.join(format!("{stem}.{ext}"));
            fs::write(&dest, rendered).with_context(|| format!("writing {}", dest.display()))?;
            Ok(())
        })
        .collect();

    let mut ok_count = 0usize;
    let mut err_count = 0usize;
    for (pdf, res) in pdfs.iter().zip(results) {
        match res {
            Ok(()) => ok_count += 1,
            Err(e) => {
                err_count += 1;
                eprintln!("error processing {}: {e:#}", pdf.display());
            }
        }
    }
    let dt = t0.elapsed();
    eprintln!(
        "Processed {ok_count}/{} PDFs ({err_count} errors) in {:.2} s ({:.2} PDF/s)",
        pdfs.len(),
        dt.as_secs_f64(),
        ok_count as f64 / dt.as_secs_f64().max(1e-9),
    );
    Ok(())
}

fn render_single_pdf(
    engine: &Engine,
    pdf: &Path,
    mode: ProcessMode,
    format: OutputFormat,
    pretty: bool,
    tei_opts: &TeiOptions,
) -> Result<String> {
    match mode {
        ProcessMode::Fulltext => {
            let doc = engine.process_pdf_file(pdf)?;
            Ok(match format {
                OutputFormat::Json => {
                    if pretty {
                        doc.to_json_pretty()?
                    } else {
                        doc.to_json()?
                    }
                }
                OutputFormat::Tei => doc.to_tei_xml(tei_opts)?,
            })
        }
        ProcessMode::Header => {
            let header = engine.process_header_file(pdf)?;
            Ok(match format {
                OutputFormat::Json => {
                    if pretty {
                        header.to_json_pretty()?
                    } else {
                        header.to_json()?
                    }
                }
                OutputFormat::Tei => header_to_tei_xml(&header, tei_opts)?,
            })
        }
        ProcessMode::References => {
            let refs = engine.process_references_file(pdf)?;
            Ok(match format {
                OutputFormat::Json => {
                    if pretty {
                        serde_json::to_string_pretty(&refs)?
                    } else {
                        serde_json::to_string(&refs)?
                    }
                }
                OutputFormat::Tei => references_to_tei_xml(&refs, tei_opts)?,
            })
        }
    }
}

fn cmd_bench(
    input: &Path,
    models: &Path,
    mode: ProcessMode,
    jobs: Option<usize>,
    iterations: usize,
) -> Result<()> {
    configure_threads(jobs)?;
    let t_load = Instant::now();
    let engine = Engine::load(models)?;
    let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;

    let pdfs = if input.is_file() {
        vec![input.to_owned()]
    } else {
        collect_pdfs(input)?
    };
    if pdfs.is_empty() {
        bail!("no PDF files found at {}", input.display());
    }

    let mut total_pages = 0usize;
    let mut total_refs = 0usize;
    let mut total_docs = 0usize;
    let mut per_doc_ms = Vec::new();

    let t_run = Instant::now();
    for _ in 0..iterations.max(1) {
        let stats: Vec<Result<(f64, usize, usize)>> = pdfs
            .par_iter()
            .map(|pdf| {
                let ts = Instant::now();
                let (pages, refs) = match mode {
                    ProcessMode::Fulltext => {
                        let d = engine.process_pdf_file(pdf)?;
                        (d.page_count, d.references.len())
                    }
                    ProcessMode::Header => {
                        let layout = nosco_pdf::extract_from_file(pdf)?;
                        let pages = layout.pages.len();
                        let _ = engine.process_header_layout(&layout)?;
                        (pages, 0)
                    }
                    ProcessMode::References => {
                        let layout = nosco_pdf::extract_from_file(pdf)?;
                        let pages = layout.pages.len();
                        let refs = engine.process_references_layout(&layout)?;
                        (pages, refs.len())
                    }
                };
                Ok((ts.elapsed().as_secs_f64() * 1000.0, pages, refs))
            })
            .collect();

        for (ms, pages, refs) in stats.into_iter().flatten() {
            per_doc_ms.push(ms);
            total_pages += pages;
            total_refs += refs;
            total_docs += 1;
        }
    }
    let wall_s = t_run.elapsed().as_secs_f64();
    per_doc_ms.sort_by(f64::total_cmp);
    let median_ms = per_doc_ms
        .get(per_doc_ms.len() / 2)
        .copied()
        .unwrap_or(0.0);
    let p95_ms = per_doc_ms
        .get((per_doc_ms.len() * 95) / 100)
        .copied()
        .unwrap_or(0.0);

    println!("=== nosco Benchmark ({mode:?}) ===");
    println!("Cold engine load : {load_ms:.1} ms");
    println!("Documents        : {total_docs} ({total_pages} pages, {total_refs} refs)");
    println!("Wall time        : {wall_s:.3} s");
    println!(
        "Throughput       : {:.2} PDF/s | {:.1} pages/s",
        total_docs as f64 / wall_s.max(1e-9),
        total_pages as f64 / wall_s.max(1e-9),
    );
    println!("Latency per PDF  : median={median_ms:.1} ms | p95={p95_ms:.1} ms");
    if let Some(rss) = eval::peak_rss_mib() {
        println!("Peak RSS (VmHWM) : {rss:.1} MiB");
    }
    Ok(())
}

fn collect_pdfs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(cur) = stack.pop() {
        for entry in fs::read_dir(&cur).with_context(|| format!("reading {}", cur.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn fmt_bytes(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.2} MiB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
