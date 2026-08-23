# unpin

`unpin` finds duplicate static-image pins in public Pinterest boards, compares
their resolutions, and prints direct pin links so you can decide what to delete.
It never changes your Pinterest account.

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
```

In the picker, arrow keys move, space toggles, enter confirms, and typing
filters the list by name. The first row selects every board at once.
With no board-selection option, every board is scanned.

Pinterest may return only part of a large board to anonymous web requests. For
a complete view, import the Pinterest session from the browser where you are
already signed in:

```console
unpin https://www.pinterest.com/username/board-name/ --cookies-from-browser chrome
unpin username/board-name --cookies cookies.txt
```

Cookie import is opt-in. `unpin` reads only Pinterest-domain cookies, uses them
in memory for the scan, and never prints or writes their values. The browser or
operating system may ask for permission to access its cookie encryption key.
Chrome, Chromium, Brave, Edge, Firefox, Arc, and Vivaldi are supported. The
currently active Chrome profile is tried first. `--cookies` accepts the standard
Netscape/Mozilla `cookies.txt` format produced by browser cookie exporters and
curl.

While running in an interactive terminal, `unpin` shows a persistent,
grouped checklist of the scan. The active step has an animated spinner; when
it completes, it remains visible with a checkmark. Concurrent stages report
their own completed and active work rather than treating launch order as
progress. The final checklist remains visible, and progress is written to
stderr, so `--format json` remains clean on stdout.

The text report separates byte-identical images from conservative visual
candidates. Within each match it marks the best-resolution pin as `KEEP` and
the others as `DELETE?`. When the best copies have identical dimensions and
file sizes, they are marked `TIE`.

Every successful run also creates a unique `unpin-*.html` comparison report in
your operating system's temporary directory and opens it in the default
browser. The report places matching images side by side with resolutions,
recommendations, and links to the original pin and image. Its path is printed
in text output and included as `visual_report` in JSON.

Useful options:

```console
# Machine-readable output
unpin https://www.pinterest.com/username/board-name/ --format json

# Suppress perceptual matches and report identical image files only
unpin https://www.pinterest.com/username/board-name/ --exact-only

# Allow a wider perceptual-hash distance (0-64, default 5)
unpin https://www.pinterest.com/username/board-name/ --similarity-threshold 8

# Create the temporary visual without opening a browser
unpin https://www.pinterest.com/username/board-name/ --no-open

# Do not create a visual report
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

Run `unpin --help` for the complete interface.

## How matching works

- Pinterest board metadata is fetched page by page, including board sections.
  Pins repeated in the main feed and a section are counted once.
- Up to twelve selected boards are fetched concurrently, and a board's sections
  are fetched up to eight at a time. All Pinterest API requests share a
  forty-eight-request ceiling. Pagination within any single feed remains
  sequential, since each page is addressed by the previous page's bookmark, so
  `unpin` asks for 250 pins per page instead of Pinterest's default 25 to keep
  that chain short. The page size is an undocumented option; if Pinterest
  refuses it, the feed is refetched at the default page size rather than
  failing. Throttled or transient requests are retried up to three times with
  bounded exponential backoff.
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
- Ordinary, single-image pins are downloaded with a forty-eight-request
  concurrency limit and a 100 MiB per-image safety limit. Decoding and hashing
  run on a separate pool sized to the machine's processors, so images keep
  downloading while earlier ones are still being analyzed.
- Successful image fingerprints are cached for 30 days in the operating
  system's user cache directory. The cache contains dimensions, file size, and
  derived hashes—not raw images, Pinterest responses, or browser cookies. Set
  `UNPIN_CACHE_DIR` to choose another cache root or use `--no-cache` to bypass
  it. Fingerprint entries live in an unpin-owned subdirectory beneath that root.
  The entry format is versioned; upgrading `unpin` past a format change means
  one full re-download before the cache is warm again.
- Identical downloaded bytes are grouped using SHA-256.
- Other images first become candidates when their 64-bit difference hashes are
  within the selected threshold and their aspect ratios differ by no more than
  one percent. A second 64×64 contrast-normalized structural comparison must
  then score at least 97%, which rejects unrelated images with similar broad
  light/dark layouts.
- Rankings use decoded pixel area, longest edge, and file size—in that order.

Story/idea pins are analyzed through their static original cover when one is
available. Videos, carousels, missing images, and failed downloads appear in the
skipped list rather than aborting an otherwise useful scan. Interactive text
groups large skipped sets by reason; JSON and the HTML report retain every
individual skipped-pin record.

## Limitations

- Anonymous scans work with public boards and public profiles. Secret boards
  and secret profiles need `--cookies-from-browser`; the board picker marks
  secret boards. Signed-in scans can use an explicitly selected local browser
  session; browser cookie values are never persisted.
- Pinterest's web resources are undocumented and may change without notice.
- Visual matches are candidates for human review, not proof that two images are
  interchangeable.
- HTML reports reference Pinterest's remote originals and require network
  access when viewed.
- Temporary reports remain after `unpin` exits so the browser can load them
  safely. The operating system eventually cleans them up; copy the printed file
  if you want to retain one.
- `unpin` only reports links. Pin deletion is always manual.

## Development

```console
cargo fmt --all -- --check
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings
```

Pull requests run this same verification gate in CI.

Tests use local mock HTTP servers and do not contact Pinterest. A live smoke
test can be added separately and should remain opt-in.
