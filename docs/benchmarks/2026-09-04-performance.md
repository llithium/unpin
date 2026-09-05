# Scan and report performance — 2026-09-04

Compared the working tree with `50c05fb1910da232faf99b2cf643b031d533aaa4` on a Mac16,10 with 10 logical CPUs, Rust 1.96.0, and Bun 1.4.0. All fixtures are local; no Pinterest requests or user cache entries were used. Rust measurements use release builds and exclude compilation. Comparison builds used separate target directories. Workloads ran sequentially.

## Results

Times below are medians of five samples, except fingerprint cloning, which uses seven. [Raw samples](2026-09-04-results.json) include every run.

| Workload | Before | After | Change |
| --- | ---: | ---: | ---: |
| Full scan, staggered sources, 96 images | 768.1 ms | 609.4 ms | 20.7% less time |
| Mixed cache, 1,536 images / 48 misses | 88.7 ms | 54.8 ms | 38.2% less time |
| Mixed cache, time to first image request | 38.8 ms | 4.5 ms | 88.4% less time |
| Fully cached scan, 1,536 images | 38.9 ms | 40.6 ms | 4.1% more time |
| 100 navigation actions, 2,000 matches | 77.8 ms | 6.2 ms | 92.0% less time |
| Clone fingerprints for 20,000 pins sharing one image | 3.70 ms | 1.93 ms | 47.7% less time |

The shared-image fixture retains **4 KiB instead of 78.125 MiB** of structural-signature payload. This counts distinct signature allocations, not total process memory; pin metadata, strings, and allocator overhead are excluded. Navigation performs **200 instead of 200,000** `aria-current` writes.

The fully cached fixture has a small measured regression of 1.6 ms. Bounded cache batches introduce scheduling overhead, but substantially reduce the mixed-cache fixture's wait before downloading. This tradeoff is retained and reported rather than treating warm-cache behavior as a speedup.

## What changed

- Structural signatures use shared immutable storage. Fingerprint clones for the same media URL no longer copy 4 KiB each. Cache serialization and format version stay compatible.
- Cache reads run in bounded batches on the blocking pool. Cache hits bypass the download queue, so a full queue of misses does not prevent cache reads from proceeding. Download, byte-budget, and CPU limits remain in place.
- Intake sends completed source batches to analysis while other sources are still collecting. Results are released in selection order, preserving which source owns a pin shared across sources. Pins move through the channel rather than being cloned into a second intake collection. Images shared by later batches are fingerprinted once.
- Report navigation reuses the filtered list and its position map and updates only the outgoing and incoming sidebar links. Review counts are maintained incrementally; reviewing one match does not rewrite every match's filter classes.
- Progress now permits collection and analysis to overlap. Reaching the currently known image count does not finish the stage until intake and image processing have actually ended.

## Fixtures and limits of the measurements

The full-scan fixture has four boards with 24 distinct media URLs each. Board responses arrive after 0, 150, 300, and 450 ms; each image response takes 150 ms. Every image has identical bytes, yielding one exact group of 96 pins. All ten comparison reports had the same normalized SHA-256:

`74edd9d07aeecb19bb330da8cc54de04f231ac142ac039d8fdc5cf353ab0f522`

Only the mock server's ephemeral origin was normalized. The benchmark checks the image-request count, analyzed count, exact-group membership count, and skipped results on every run.

Intake overlap is at **source boundaries**, not individual pages. It helps when an earlier selected source finishes while later sources are still loading. A single board, or a slow first selected source, will benefit less. The timings demonstrate overlap under controlled latency; they do not predict a fixed speedup for live Pinterest scans.

The cache fixture seeds a fresh temporary directory for each sample. All 1,536 fingerprints identify the same image; the mixed variant omits the first 48 entries and gives image responses a 40 ms delay. Each run verifies 1,536 analyzed pins, one exact group, no visual candidates, no skips, and the expected download count. This measures warm/mixed cache behavior, not structural-matching throughput across unrelated images.

The browser fixture repeats an existing match and sidebar entry to make 2,000 unique review groups. It runs five warmup navigation actions and times 100 synchronous navigation actions, including state persistence. Both versions use identical markup and fixtures, swapping only the report script. It checks the final selected match and exactly one active sidebar link. These timings measure JavaScript work, not browser paint or initial report load; the report still creates its full DOM.

Fingerprint-cloning timings include pin-to-analysis conversion but exclude fixture construction and destruction. Allocator warmup affects individual samples; the storage reduction is the stronger result.

## Reproduce

Build before collecting timings and run one workload at a time:

```sh
cargo build --locked --release --example benchmark_scan
cargo test --locked --release benchmark_ --no-run
cargo run --locked --release --example benchmark_scan -- --runs 5
cargo test --locked --release benchmark_shared_signatures -- --ignored --nocapture
cargo test --locked --release benchmark_cache_pipeline -- --ignored --nocapture
cargo run --quiet --locked --example render_test_reports -- test-results/visual-report.html
bun tests/browser/benchmark-report.mjs
```

For baseline Rust measurements, copy `examples/benchmark_scan.rs` and `src/analysis_benchmarks.rs` into a clean archive of the baseline revision and append this test-only declaration to its `src/analysis.rs`:

```rust
#[cfg(test)]
#[path = "analysis_benchmarks.rs"]
mod benchmarks;
```

Use a separate Cargo target directory for each checkout to avoid stale build artifacts. For the browser baseline, reuse the current fixture and pass the old template as the second argument:

```sh
bun tests/browser/benchmark-report.mjs test-results/visual-report.html /path/to/baseline/templates/report.html
```

## Validation

- 151 Rust unit tests and 19 end-to-end tests passed; four manual benchmarks are ignored by the normal suite.
- Seven Chromium tests passed, covering navigation, composed filters, review restoration/reset, unavailable storage, images, responsive layouts, and printing.
- Focused checks cover early image requests while a later source is unavailable, one download for a URL shared across batches, source ownership despite out-of-order completion, progress during overlapping stages, and bounded sidebar updates.
- Strict Clippy, Rust formatting, and `git diff --check` passed.
