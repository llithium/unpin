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
