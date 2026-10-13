# Review decision navigation — 2026-10-12

Compared the report script from `7d3fe51` with the implementation in this working
change, using the same newly rendered HTML and 2,000 expanded matches. Both
versions received the same long, content-shaped review keys. Five sequential
samples each timed 100 synchronous navigation actions after five warmup actions.

| Measurement | Before | After |
| --- | ---: | ---: |
| Median JavaScript time for 100 navigations | 85.8 ms | 60.4 ms |
| `aria-current` writes per 100 navigations | 200 | 200 |

This fixture took 29.6% less JavaScript time. View preferences now have a small
separate storage record; navigation no longer serializes the entire group list
or decision history. Decision history is written on edits and merged with other
open reports. The benchmark checks the final selected match and exactly one
active sidebar link. [Raw samples](2026-10-12-report-navigation.json) retain every
run.

These measurements exclude browser painting, image loading, and initial report
setup. No decision history was preseeded. They demonstrate this controlled
navigation path, not an overall scan speedup. The older September measurements
used shorter generated review keys, so those timings are not directly comparable.

Reproduce after building the fixture, one workload at a time:

```console
cargo run --quiet --locked --example render_test_reports -- test-results/visual-report.html
bun tests/browser/benchmark-report.mjs
```

For an identical-DOM baseline comparison, save the old template outside the
checkout and pass its path as the second argument. The helper accepts both the
old inline script and the new embedded `report.js` template include.
