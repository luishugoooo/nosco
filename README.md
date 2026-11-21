<img src="assets/icon.svg" width="48" height="48" alt="nosco">

# nosco

Pure-Rust scholarly PDF and bibliographic extraction engine compatible with [GROBID](https://github.com/grobidOrg/grobid) 0.9.1 CRF models. Compared to upstream `grobid/grobid:0.9.1-crf`, it runs ~13× faster (8.4 vs. 0.66 PDF/s) with ~13× less RAM (410 MiB vs. 5.2 GiB) and a 42 ms cold start (vs. 30 s) by running PDF layout, feature extraction, and compact binary CRF models (~61× smaller) in a single native process with no JVM, `pdfalto` subprocess, or JNI/XML round-trips.

## Usage

Build and convert an upstream `grobid-home` directory into a compact binary asset pack (`.gcrf` models + `.glex` lexicons, ~19.8 MiB):

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

140 scholarly PDFs (3,146 pages; arXiv, Nature OA, PLOS, BMC/Springer, OpenAlex), 4 CPU cores, no consolidation:

| Metric | `nosco` | `grobid/grobid:0.9.1-crf` |
| :--- | ---: | ---: |
| Cold start | 41.7 ms | 30.2 s |
| Wall time | 16.6 s (8.4 PDF/s) | 212.3 s (0.66 PDF/s) |
| Median / p95 latency | 360 ms / 1.34 s | 4.21 s / 13.51 s |
| Peak RSS | 410.5 MiB | 5,158.9 MiB |
| Title mean sim (blank) | 98.3% (0) | 96.9% (2) |
| Author F1 | 93.2% | 94.5% |
| Abstract F1 | 89.8% | 86.5% |
| Publisher DOIs | 50/50 | 45/50 |
| References | 7,155 | 7,098 |

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
