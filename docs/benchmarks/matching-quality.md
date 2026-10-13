# Matching quality baseline

The `synthetic-scenes-v1` collection contains 15 labeled pairs: three hand-designed
scenes, each compared with a resized image, JPEG recompression, a substantial
crop, a caption overlay, and different objects in a similar overall layout.
The deterministic generator is in `src/matching_quality.rs`; no downloads,
Pinterest credentials, or third-party image rights are needed.

Labels describe interchangeability: resizing and recompression preserve content;
substantial crops, captions, and changed objects do not. These are deliberately
conservative cleanup labels, not a claim that related images are unrelated.

At the existing hash threshold of 5 and structural threshold of 0.97:

| Outcome | Pairs |
| --- | ---: |
| Correct matches | 6 |
| Correct rejections | 9 |
| False positives | 0 |
| Missed matches | 0 |

These numbers characterize this small synthetic set only. They do not estimate
accuracy on real photographs, illustrations, screenshots, or Pinterest libraries.
The baseline should grow with labeled examples of actual matching mistakes
before changing the thresholds. Thresholds were not changed by this work.

The test exercises image encoding, decoding, hashing, aspect-ratio checks,
structural comparison, and candidate generation. Each row in
`matching-quality.json` records its label, result, hash distance, and structural
score. The test checks both false positives and missed positives.

To reproduce the recorded results:

```console
UNPIN_QUALITY_RESULTS=docs/benchmarks/matching-quality.json cargo test --locked labeled_matching_quality_baseline -- --nocapture
```

## Real-image and screenshot baseline

`real-images-v1` adds 27 labeled pairs from three photographs, a scanned text
sample, and two screenshots of the actual report. Together with the 15 synthetic
pairs above, the repository now evaluates 42 pairs. The screenshot negative keeps
the interface identical while replacing the photograph, exercising a similar
layout with different meaningful content.

At the unchanged thresholds, the real-image set produces:

| Outcome | Pairs |
| --- | ---: |
| Correct matches | 10 |
| Correct rejections | 17 |
| False positives | 0 |
| Missed matches | 0 |

These remain small, curated transformation examples. They do not estimate
accuracy on a Pinterest library or establish that a matching cover means its
underlying video/carousel is interchangeable. Substantial crops and captions
remain non-interchangeable under the conservative labeling policy.

The assets, explicit pair labels, reasons, source provenance, and SHA-256 hashes
are in `tests/fixtures/matching/corpus.json`. The evaluator checks every asset's
hash before decoding it. See the adjacent README for rights and capture details.
[Raw results](real-matching-quality.json) include individual hash distances and
structural scores. The regression gate separately checks false positives and
missed duplicates against the manifest's reviewed baseline; production matching
thresholds were not changed.

Reproduce offline:

```console
UNPIN_REAL_QUALITY_RESULTS=docs/benchmarks/real-matching-quality.json cargo test --locked labeled_real_image_matching_baseline -- --nocapture
```

To evaluate local examples of actual mistakes, copy the manifest and add assets
and labeled pairs. An asset records its local `file`, SHA-256, author, license,
category, and source. A pair supplies an ID, left/right asset IDs with a transform,
`expected_match`, and a labeling reason. `original` on both sides compares two
independently supplied images; other supported transforms are `resized`,
`recompressed`, `crop`, and `caption_overlay`.

```console
UNPIN_QUALITY_CORPUS=/absolute/path/corpus.json cargo test --locked labeled_real_image_matching_baseline -- --nocapture
```

Exported report decisions include `not_duplicate` outcomes, image URLs, and image
hashes to help locate examples. They are review evidence, not automatically
trusted quality labels: inspect the actual content, store the selected images
locally with permission/provenance, and add explicit labels to a corpus manifest.
Evaluation never downloads exported URLs.
