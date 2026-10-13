# Matching corpus provenance

These fixtures are committed so normal tests never need network access. Asset
SHA-256 hashes, sources, and pair labels are recorded in `corpus.json`. The tests
exercise real-image transformations and screenshots alongside the existing
synthetic drawing baseline; they are not a random sample of Pinterest pins.

| Asset | Origin | Rights documented by source |
| --- | --- | --- |
| `astronaut.png` | NASA photograph of Eileen Collins, distributed in scikit-image v0.25.2 | Public domain |
| `camera.png` | Lav Varshney photograph, scikit-image v0.25.2 | CC0 |
| `chelsea.png` | Stefan van der Walt photograph, scikit-image v0.25.2 | CC0 |
| `text.png` | Wikipedia `Corner.png`, distributed in scikit-image v0.25.2 | Public domain |
| `report.png` | Screenshot of this repository's synthetic report with the NASA photograph | Repository-authored interface; embedded photograph public domain |
| `report-changed.png` | Same report with the Lav Varshney photograph | Repository-authored interface; embedded photograph CC0 |

The scikit-image project's [official data documentation](https://scikit-image.org/docs/0.25.x/api/skimage.data.html)
documents the photograph and text-image rights. The manifest links each original
asset to its pinned upstream v0.25.2 source. No license is added to the existing
unpin interface by including its own test screenshots here.

Screenshot fixtures were captured through Chromium from the actual standalone
report, with image requests fulfilled from the committed photographs. They are
fixed matching examples, not visual snapshots that tests regenerate. To
intentionally recapture them after reviewing the new content:

```console
bun tests/browser/capture-quality-fixtures.mjs
UNPIN_REAL_QUALITY_RESULTS=docs/benchmarks/real-matching-quality.json cargo test --locked labeled_real_image_matching_baseline -- --nocapture
```

The capture helper updates the screenshot hashes and rejects identical files for
the changed-content pair. Recapturing must retain the intended labels; do not
increase baseline error allowances merely to make a new failure pass.
