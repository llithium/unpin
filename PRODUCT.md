# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Pinterest curators who have accumulated saved images across one or more boards and need to find redundant saves, compare copies, and decide what to keep or delete. They may work from a board or profile and need a reviewable result without the product taking action on their account.

## Product Purpose

`unpin` scans Pinterest boards or profiles for duplicate static-image pins. It compares byte-identical images and conservative visual matches, surfaces the best-resolution copy, and gives the curator direct pin and image links with a human-review recommendation. Success is a trustworthy shortlist that lets the user clean up manually; the tool never changes the Pinterest account.

## Positioning

The product pools selected boards into one analysis, so a duplicate saved in separate boards is visible in the same run. It distinguishes exact duplicates from visual candidates and labels matches by same-board versus across-board scope, giving the user the context needed to decide whether a duplicate is actually redundant.

## Operating Context

- Runs as a Rust command-line tool against a board URL, `username/board` shorthand, username, or profile URL.
- A profile can be scanned across all boards, a named subset, or an interactive keyboard board picker; same-board and cross-board scopes can be filtered.
- Anonymous public scans are supported. A signed-in browser session or exported Pinterest-domain cookie file can be provided when Pinterest limits anonymous results or when secret boards/profiles need to be scanned. Cookie values remain in memory and are not printed or written.
- By default, a run creates a temporary HTML comparison report and opens it in the browser; `--no-visual` produces text or JSON. The browser comparison is the primary future-facing review surface, while terminal and JSON output remain supported product surfaces.
- Reports remain local and link to Pinterest originals. The final decision and any pin deletion happen manually in Pinterest.

## Capabilities and Constraints

- Accepts board and profile targets, board selection, exact-only matching, configurable visual-similarity thresholds, same-board or cross-board filters, text/JSON output, HTML report controls, interactive progress, and optional local cookie import.
- Groups byte-identical images with SHA-256. Other images become conservative candidates only after perceptual and structural checks; matches are ranked by decoded pixel area, longest edge, and file size.
- Reports `KEEP`, `DELETE?`, or `TIE` recommendations based on the comparison. Visual matches are candidates for human review, not proof that two images are interchangeable.
- Story/idea pins can be analyzed through a static original cover when available. Videos, carousels, missing images, and failed downloads are retained as skipped-pin outcomes rather than aborting the whole scan.
- Successful fingerprints are cached locally for 30 days. The cache stores derived metadata and hashes, not raw images, Pinterest responses, or browser cookies. A per-image download safety limit and bounded request/retry behavior protect the scan from unbounded work.
- Pinterest web resources are undocumented and may change without notice. HTML reports reference remote images and pin links, so viewing them still requires network access.
- Pin deletion is never automated; `unpin` reports links and recommendations only.
- Repository terminology: a **Scan** is one run of `unpin`; a **scan source** is a board or profile feed contributing pins; **scan intake** resolves targets and collects sources; a **progress step** is a named stage whose completion remains visible in the run history.

## Brand Commitments

- The product name is `unpin`, used in lowercase.
- The product is explicit about user control and uncertainty: it never changes a Pinterest account, and visual suggestions are marked for review rather than presented as facts.
- Existing output favors direct, operational language such as `KEEP`, `DELETE?`, `SAME BOARD`, and `ACROSS BOARDS`.

## Evidence on Hand

- `README.md` documents the product promise, command usage, report behavior, matching model, privacy handling, limitations, and development checks.
- `src/` contains the Rust CLI, Pinterest intake/authentication, duplicate analysis, progress, reporting, and HTML rendering implementation.
- `templates/report.html` and `templates/item.html` contain the current browser comparison report and its item-level markup.
- `tests/end_to_end.rs` and module tests provide local mock-server and behavior coverage; the test suite does not contact Pinterest.
- No testimonials, customer logos, benchmarks, press, or other external proof are present in the repository. Future work must not fabricate them.

## Product Principles

- Preserve the curator’s control: report and explain; never mutate Pinterest state.
- Compare the whole selected context: duplicates across boards matter as much as duplicates within one board.
- Separate certainty from suggestion: exact matches, visual candidates, incomplete scans, and skipped pins must remain distinguishable.
- Make review actionable: show the relevant board, dimensions, quality ranking, and direct pin/image links.
- Be transparent about data handling and technical limits so users know what the scan did and did not establish.
