# unpin

`unpin` finds duplicate visual pins in public Pinterest boards, compares their
resolutions, and walks you through removing the copies you don't want on
Pinterest. It never changes your Pinterest account.

The program is written in Rust and talks directly to the same undocumented
Pinterest web resources used by Pinterest's site. It does **not** invoke or
depend on gallery-dl.

## Install

Build from source with a current Rust toolchain:

```console
cargo install --path .
```

## Use

Pass a board URL or `username/board` shorthand directly:

```console
unpin https://www.pinterest.com/username/board-name/
unpin username/board-name
```

Pass a username or profile URL to scan every board in the account:

```console
unpin username
unpin https://www.pinterest.com/username/
```

The boards are analyzed **together**, so a photo saved to two different boards
is reported as a duplicate—something a board-at-a-time scan cannot find. Every
match then shows which board each pin came from.

Use `--interactive` to open the board picker, or `--boards` to select board
slugs or names (comma-separated or repeated):

```console
unpin username --boards board-name,Another Board
unpin username --interactive
unpin username --unorganized
unpin username --boards board-name --unorganized
```

In the picker, arrow keys move, space toggles, enter confirms, and typing
filters the list by name. The first row selects every board at once.
`--boards` by itself scans only the named boards. `--unorganized` by itself
scans only pins saved directly to the profile as Unorganized ideas; combined
with `--boards`, it adds those pins to the selected boards. With no
board-selection option, every board and Unorganized ideas are scanned.

Pinterest may return only part of a large board to anonymous web requests. For
a complete view, import the Pinterest session from the browser where you are
already signed in:

```console
unpin https://www.pinterest.com/username/board-name/ --cookies-from-browser chrome
unpin username/board-name --cookies cookies.txt
```

Cookie import is opt-in. `unpin` reads only Pinterest-domain cookies, uses them
in memory for the scan, and never prints or writes their values. Imported-cookie
scans require an HTTPS Pinterest URL; anonymous scans can still use HTTP targets
when no cookies are imported. The browser or operating system may ask for
permission to access its cookie encryption key. Chrome, Chromium, Brave, Edge,
Firefox, Arc, and Vivaldi are supported. The currently active Chrome profile is
tried first. `--cookies` accepts the standard Netscape/Mozilla `cookies.txt`
format produced by browser cookie exporters and curl.

While running in an interactive terminal, `unpin` shows a persistent,
grouped checklist of the scan. The active step has an animated spinner; when
it completes, it remains visible with a checkmark. Concurrent stages report
their own completed and active work rather than treating launch order as
progress. The final checklist remains visible, and progress is written to
stderr, so `--no-visual --format json` remains clean on stdout. Paginated
requests share one rolling status row, keeping large scans readable while
still showing the latest resource, page, and item count.

By default, every successful run starts a small review server on your own
computer (`127.0.0.1`, port 7171 when it is free) and opens the review page in
your browser. The page places matching visual previews side by side with
resolutions, recommendations, and links to the original pin and preview. Keep
`unpin` running while you review, and press Ctrl-C when you are done. The
review address appears as a completed progress step on stderr; it includes a
random token, and the server refuses requests from other websites.

### Removing duplicates on Pinterest

Pinterest has no way for other apps to delete your pins, so removal happens on
Pinterest itself. The review page splits the work into two steps, shown at the
top of the page.

**1 · Decide.** Matches appear as a gallery of stacked cards, starting with
duplicates on the same board. Copies on different boards are often saved on
purpose, so they sit under **Across boards** and never count as easy calls.

- **Easy calls** are the same image, or a 98% or closer match, saved more than
  once on one board. **Accept all** keeps the largest copy of each and puts the
  rest in the removal tray in one step, or choose **Look through them first**.
  Covers of video and story pins are never easy calls.
- Every card has **Remove smaller** (or **Remove duplicate**) and **Keep both**
  buttons, so most matches take one tap without opening them. Once you decide,
  the buttons give way to what you chose (for example **Removing 1 copy**) and
  a **Change** link; after you remove the copy on Pinterest the card says
  **Removed from Pinterest**.
- Open a card to compare its copies side by side. The copy that **Remove the
  smaller copy** would take is marked **Suggested to remove**. That button and
  **Keep both** save and move to the next match; `R` and `A` do the same, and
  `J` and `K` move between matches. To choose a copy yourself, tap **Remove this
  one** on it; that saves straight away too. `O` or `Escape` goes back to the
  gallery.
- Everything flagged collects in the removal tray at the bottom of the page.

**2 · Remove.** Choose **Start removing** on the tray.

1. The page shows one pin at a time, large, with the copy that stays.
2. Choose **Open on Pinterest**. Pins always open in the same Pinterest tab, so
   tabs don't pile up. Delete the pin there from its ⋯ menu.
3. Come back to the review page. `unpin` asks Pinterest whether the pin still
   exists, ticks it off when it is gone, and shows the next one. **Keep this
   one** takes a pin out of the tray instead. **Check all with Pinterest**
   rechecks the whole tray, for example after removing pins on your phone.

Checks use the same Pinterest session as the scan. Signed out, a pin on a
secret board can look removed, so scan with `--cookies-from-browser` for
reliable checks. You can always mark a pin removed yourself, or undo a mark.
The review page warns when every copy in a match is flagged.

Decisions and removal marks are saved to `review.json` in your user data folder
(`~/Library/Application Support/unpin` on macOS, `%APPDATA%\unpin` on Windows,
`$XDG_DATA_HOME/unpin` or `~/.local/share/unpin` elsewhere), or in
`UNPIN_DATA_DIR` when it is set. The file uses the same format as
`Export decisions`, so earlier exports can be imported on the review page.

Use `--static` to write a standalone `unpin-*.html` report to your temporary
directory instead of starting the server. A standalone report has the same
steps and tray, but keeps decisions in that browser and asks you to mark each pin
removed yourself, because a file opened from disk cannot ask Pinterest. HTML and
CLI reports are mutually exclusive; use `--no-visual` to print the CLI report
instead.

The CLI report separates byte-identical images from conservative visual
candidates. Within each match it marks the best-resolution pin as `KEEP` and
the others as `DELETE?`. When the best copies have identical dimensions and
file sizes, they are marked `TIE`.

Useful options:

```console
# Machine-readable output
unpin https://www.pinterest.com/username/board-name/ --no-visual --format json

# Suppress perceptual matches and report identical image files only
unpin https://www.pinterest.com/username/board-name/ --exact-only

# Allow a wider perceptual-hash distance (0-64, default 5)
unpin https://www.pinterest.com/username/board-name/ --similarity-threshold 8

# Start the review server without opening a browser
unpin https://www.pinterest.com/username/board-name/ --no-open

# Write a standalone HTML report instead of starting the review server
unpin https://www.pinterest.com/username/board-name/ --static

# Use the CLI report instead of creating a visual report
unpin https://www.pinterest.com/username/board-name/ --no-visual

# Suppress interactive progress
unpin https://www.pinterest.com/username/board-name/ --no-progress

# Suppress ANSI colors in interactive text output
unpin https://www.pinterest.com/username/board-name/ --no-color

# Ignore cached image fingerprints and download every image again
unpin https://www.pinterest.com/username/board-name/ --no-cache

# Use a signed-in browser session when Pinterest truncates anonymous results
unpin https://www.pinterest.com/username/board-name/ --cookies-from-browser chrome

# Report only duplicates saved more than once within one board
unpin username --same-board-only

# Report only duplicates that span different boards
unpin username --cross-board-only
```

Save a permanent HTML report, or save JSON and review it later without scanning
Pinterest again:

```console
unpin username --output review.html --no-open
unpin username --no-visual --format json --output scan.json
unpin render scan.json
unpin render scan.json --output review.html --no-open
```

`unpin render scan.json` starts the review server. It checks removed pins with a
signed-out Pinterest connection; add `--offline` to skip checks entirely.

Output files are written atomically and existing files are never overwritten.
New JSON reports carry `schema_version: 2`, including media types, stable source
identities, matching-work counters, and structured `collection_issues` on affected
boards. The render command accepts versions 1 and 2 and rejects unversioned or
unsupported versions. Version 1 items without media metadata are labeled unknown
and require content inspection instead of receiving deletion recommendations.
Rendering needs no Pinterest session,
although viewing the remote preview images still needs network access.

Run `unpin --help` for the complete interface.

## How matching works

- Pinterest board metadata is fetched page by page, including board sections.
  Pins repeated in the main feed and a section are counted once.
  If section discovery or an individual section feed fails, the main board
  feed and any successful sections are still analyzed, with the unavailable
  section reported as a warning.
- Up to forty-eight selected boards are fetched concurrently, and a board's
  sections are fetched up to sixteen at a time. All Pinterest API requests
  share a 128-request ceiling. Pagination within any single feed remains
  sequential, since each page is addressed by the previous page's bookmark, so
  `unpin` asks for 250 pins per board or profile page and 50 per section page
  instead of Pinterest's default 25 to keep that chain short. The page size is
  an undocumented option; if Pinterest refuses it, the feed is refetched at
  the default page size rather than failing. A board's main feed and section
  feeds, and a profile's unorganized feed and board feeds, overlap when they
  are independent. Throttled or
  transient requests are retried up to three times with bounded exponential
  backoff.
- When several boards are selected, their pins are pooled into a single
  analysis, so duplicates spanning two boards are found. Each reported pin
  carries its board name, and pin counts are also broken down per board.
- Each match is then tagged `SAME BOARD` or `ACROSS BOARDS`. The distinction
  matters when deciding what to delete: the same image saved twice into one
  board is a redundant double-save, while the same image in two boards is often
  deliberate and worth a second look. JSON carries this as `scope`
  (`same_board` or `cross_board`) on every exact group and visual candidate.
  The tag is omitted when only one board was scanned, since every match is
  then same-board by definition.
- When Pinterest's reported total is larger than the number returned by its web
  API, text, JSON, and HTML output show both counts and include an incomplete
  scan warning.
- Image analysis starts as selected sources finish, while later sources are
  still loading. Its progress total can grow as more images are discovered.
- Static image previews are downloaded with a forty-eight-request concurrency
  limit and a 100 MiB per-image safety limit. Decoding and hashing run on a
  separate pool sized to the machine's processors, so previews keep downloading
  while earlier ones are still being analyzed.
- Successful image fingerprints are cached for 30 days in the operating
  system's user cache directory. The cache contains dimensions, file size, and
  derived hashes—not raw images, Pinterest responses, or browser cookies. Set
  `UNPIN_CACHE_DIR` to choose another cache root or use `--no-cache` to bypass
  it. Fingerprint entries live in an unpin-owned subdirectory beneath that root.
  The entry format is versioned; upgrading `unpin` past a format change means
  one full re-download before the cache is warm again.
- The review page stores decisions for unchanged matches across board selections
  and board renames: in `review.json` when served by `unpin`, or in the browser
  for a standalone report. Decisions include reviewed, keep all saves,
  decide later, not a duplicate, and selected copies flagged for removal. Removal
  flags stay editable until you choose `Save decision`; `Mark reviewed` marks a comparison
  reviewed, and `Keep all` records that choice directly. `More` contains
  decide-later, not-duplicate, and clear-decision actions. Decide-later matches
  stay in the unreviewed queue. Changed image content, media types, or saved-pin
  membership produce a new review item.
  `Flag for removal` saves a draft selection; `Save decision` completes its review.
  Confirmation messages distinguish those two states. Flagged copies are listed
  under `Remove`, where you remove them on Pinterest. Board names can be expanded by
  clicking, tapping, or pressing Enter to read the full name on narrow screens.
  Pin IDs and media types are always visible below each preview; backup and scan details
  are under `Report tools`. `Export decisions` backs up the browser's decision history as JSON, including
  pin/image links, image fingerprints, and media types. `Import decisions`
  validates the whole file, merges matching decisions, and retains decisions for
  matches outside the current report. Imports, resets, and decision changes can
  be undone while the confirmation is visible. Confirmations dismiss after five
  seconds, pause while hovered or keyboard-focused, and can be closed with × or
  Escape. Reset clears only decisions in the current report; other open
  reports merge changes instead of overwriting unrelated decisions.
  Review storage from older versions resets once because the identities and
  decision format changed. For standalone reports, browser handling of
  local-file storage varies; use export/import when moving browsers, machines,
  or isolated local-file storage.
  If storage is unavailable, a notice explains that progress lasts only for the
  current session. Export still works.
- Identical downloaded bytes are grouped using SHA-256.
- Other images first become candidates when their 64-bit difference hashes are
  within the selected threshold and their aspect ratios differ by no more than
  one percent. A second 64×64 contrast-normalized structural comparison must
  then score at least 97%, which rejects unrelated images with similar broad
  light/dark layouts.
- Visual matching stops at 250,000 candidate-pair checks, 2,000,000 hash-tree
  node visits, or the report-size limit of 100,000 candidates / 200,000 saved-pin
  references. Rejected pairs also consume the work budget, and hash queries
  stream results rather than allocating entire candidate lists. The report
  retains exact matches and visual comparisons found so far, prominently marks
  visual matching incomplete, and records its counters and stop reason in JSON.
  Narrow selected boards, use `--exact-only`, or lower `--similarity-threshold`
  to reduce matching work.
- Byte-identical saves of each visual variant share one comparison per board
  pair. Each side shows one preview and expandable links to its additional
  saves. JSON retains representatives in `items` and additional saves in the
  corresponding `copies` arrays. Each comparison is directly verified; similar
  images are never merged through transitive matches.
- Rankings use decoded pixel area, longest edge, and file size—in that order.

If pagination fails after successful pages, board, section, and profile pin
feeds retain the collected results and report the failed resource, number of
successful pages, and reason. Successful sections also survive a failed main
board feed. Incomplete collection is called out in text, HTML, and JSON; a
failure before any usable results still fails that source. Scans do not resume
bookmarks across runs.

Story/idea pins are analyzed through their static original cover when one is
available. Video pins use their static preview or poster, and carousel pins use
the first valid carousel slide as their representative preview. Pins without a
usable preview and failed downloads appear in the skipped list rather than
aborting an otherwise useful scan. Media type is retained through saved reports.
Any match containing a video poster, story cover, carousel cover, or unknown
media type is labeled as matching covers and uses `INSPECT` instead of keep/delete
recommendations. Such matches are never easy calls. The structural
similarity percentage is a comparison score, not a probability that deletion is
safe. Interactive text groups large skipped sets by reason; JSON and the HTML report retain every individual skipped-pin record.

## Limitations

- Anonymous scans work with public boards and public profiles. Secret boards
  and secret profiles need `--cookies-from-browser`; the board picker marks
  secret boards. Signed-in scans can use an explicitly selected local browser
  session; browser cookie values are never persisted.
- Pinterest's web resources are undocumented and may change without notice.
- Visual matches are candidates for human review, not proof that two pins are
  interchangeable. Video content itself is not decoded, and a carousel is
  represented by its first valid slide.
- HTML reports embed their controls and styling, but reference Pinterest's
  remote originals and pin links, so network access is still required when
  viewing images or opening those links.
- The review page works only while `unpin` is running. Standalone reports from
  `--static` remain after `unpin` exits, until the operating system cleans up
  its temporary files; use `--output` to retain a report in a location you
  choose.
- Pin deletion is always manual, on Pinterest. Pinterest's official API only
  lets approved apps delete pins, so `unpin` opens each pin for you and checks
  afterwards instead.

## Development

```console
cargo fmt --all -- --check
cargo test --locked --all-targets --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
```

The report UI also has browser tests. With Bun installed, prepare Chromium once
and run them with:

```console
bun install --frozen-lockfile
bunx playwright install chromium
bun run test:browser
```

The browser suite renders synthetic reports through the Rust template, serves
them locally, and supplies local image responses. A second suite runs the
review server against a stub Pinterest to cover saving decisions to a file,
reusing one Pinterest tab, and confirming removals. It exercises filtering,
navigation, review persistence, and responsive presentation without a Pinterest
account. Its fixture generator is a Cargo example and is not installed with the
CLI.

Pull requests run both the Rust and browser verification gates in CI.

Tests use local mock HTTP servers and do not contact Pinterest. A live smoke
test can be added separately and should remain opt-in.

The [matching-quality baselines](docs/benchmarks/matching-quality.md) cover 15
synthetic pairs and 27 pairs based on real photographs, scanned text, and report
screenshots. They exercise resizing, recompression, crops, captions, and changed
content in similar layouts. Run both with `cargo test --locked labeled_`.
Fixtures are checked in with provenance and SHA-256 hashes; evaluation is offline.
A local `UNPIN_QUALITY_CORPUS` manifest can add labeled examples of actual review
mistakes without changing production thresholds.
