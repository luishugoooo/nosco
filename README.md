<img src="assets/icon.svg" width="48" height="48" alt="nosco">

# nosco

Pure-Rust scholarly PDF and bibliographic extraction engine compatible with [GROBID](https://github.com/grobidOrg/grobid) 0.9.1 CRF models. It runs the full pipeline in one native process and processes PDFs about 10× as fast as upstream GROBID in the benchmark below.

v0.2.0 improves header extraction with embedded PDF metadata fallbacks, adds support for editors, organizational authors, and ISBNs, and speeds up PDF processing. Header-only mode loads only the models it needs.

## Usage

Download and unpack a [release binary and the GROBID 0.9.1 model pack](https://github.com/luishugoooo/nosco/releases/latest). Run the binary with `--models` pointing to the unpacked model directory.

To build from source and convert an upstream `grobid-home` into a compact model pack:

```bash
cargo build --release -p nosco-cli
./target/release/nosco setup --grobid-home /path/to/grobid-home --out ./assets
```

Process PDFs or raw bibliographic strings:

```bash
# Fulltext -> JSON or TEI P5 XML
./target/release/nosco process paper.pdf --models ./assets --format json --pretty
./target/release/nosco process paper.pdf --models ./assets --format tei --coords

# Directory batch / header-only
./target/release/nosco process ./pdfs/ --models ./assets --output ./out/ --jobs 4
./target/release/nosco process paper.pdf --models ./assets --mode header

# Raw parsers
./target/release/nosco parse-citation --models ./assets "A. Vaswani et al., Attention Is All You Need, NeurIPS, 2017."
./target/release/nosco parse-names --models ./assets "John A. Smith and Marie Curie"
./target/release/nosco parse-affiliation --models ./assets "Stanford University, Stanford, CA, USA"
./target/release/nosco parse-date --models ./assets "March 14, 2023"
```

As a Rust library:

```rust
use std::path::Path;
use nosco_engine::Engine;
use nosco_models::TeiOptions;

let engine = Engine::load(Path::new("./assets"))?;
let doc = engine.process_pdf_file(Path::new("paper.pdf"))?;
let json = doc.to_json_pretty()?;
let tei = doc.to_tei_xml(&TeiOptions::default())?;
```

## Benchmark

Full-text extraction on 140 scholarly PDFs (3,146 pages; arXiv, Nature OA, PLOS, BMC/Springer, OpenAlex), with four workers on an AMD EPYC Rome VM and consolidation disabled. Extraction scores and reference counts use the 139 PDFs with valid reference metadata.

| Metric | `nosco` v0.2.0 | `grobid/grobid:0.9.1-crf` |
| :--- | ---: | ---: |
| Startup | 315 ms | 38.6 s |
| Wall time | 18.7 s (7.49 PDF/s) | 194.5 s (0.72 PDF/s) |
| Median / p95 latency | 403 ms / 1.42 s | 3.83 s / 12.60 s |
| Peak memory | 346.0 MiB | 5,464.1 MiB |
| Title mean similarity (blank) | 98.0% (1) | 96.9% (2) |
| Author F1 | 92.8% | 94.5% |
| Abstract F1 | 89.6% | 86.5% |
| Publisher DOIs | 50/50 | 45/50 |
| References | 7,115 | 7,098 |

Startup measures nosco model load and GROBID container readiness. Memory measures nosco process peak RSS and GROBID peak sampled container usage.

## Why It's Fast

- **Single-process pipeline** — PDF parsing, layout analysis, feature extraction, and CRF decoding run in memory without spawning `pdfalto`, writing intermediate XML to disk, or crossing JVM/JNI boundaries.
- **Compacted binary models & lexicons** — `nosco setup` strips L1-zeroed observations from upstream Wapiti text models (182.2 MiB → 2.99 MiB `.gcrf`) and packs lexicons into a flat-arena `.glex` binary trie, avoiding startup decompression and text parsing.
- **Flat memory layout & scratch reuse** — tokens, trie nodes, and CRF weights live in contiguous arrays without per-object JVM/GC overhead, and workers reuse a single `DecodeScratch` buffer across sequences and documents.
- **No raster image decoding** — `nosco-pdf` keeps image bounding boxes for layout and figure detection and discards raw image stream bytes immediately.
- **Shared read-only weights** — immutable models and lexicons are shared (`Send + Sync`) across Rayon threads for both multi-PDF batches and parallel citation parsing.

## Crates

- `nosco-crf` — Wapiti-compatible linear-chain CRF decoder and `.gcrf` binary format
- `nosco-lexicon` — tokenizer, prefix trie matcher, and `.glex` binary cache
- `nosco-pdf` — PDF content interpreter (`lopdf`), CMap/font decoding, XY-cut layout (`#![forbid(unsafe_code)]`)
- `nosco-features` — feature extractors for all 11 GROBID CRF models
- `nosco-models` — document structs and JSON / TEI P5 XML serializers
- `nosco-engine` — fulltext cascade, raw string parsers, callout and affiliation linking
- `nosco-cli` — `nosco` CLI (`setup`, `process`, `parse-*`, `eval`, `bench`)

## Limitations

Born-digital PDFs only (no OCR), CRF models only (no DeLFT/GPU models), and local CLI/library only (no HTTP server or external Crossref consolidation).

## License

[Apache-2.0](LICENSE). See [NOTICE](NOTICE) for upstream GROBID/Wapiti and bundled font/glyph table attributions. `nosco` is an independent project and is not affiliated with or endorsed by the GROBID project.
