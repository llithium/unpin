# unpin

`unpin` finds duplicate static-image pins in a public Pinterest board, compares
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

Pass a board URL directly:

```console
unpin https://www.pinterest.com/username/board-name/
```

Pinterest may return only part of a large board to anonymous web requests. For
a complete view, import the Pinterest session from the browser where you are
already signed in:

```console
unpin https://www.pinterest.com/username/board-name/ --cookies-from-browser chrome
```

Cookie import is opt-in. `unpin` reads only Pinterest-domain cookies, uses them
in memory for the scan, and never prints or writes their values. The browser or
operating system may ask for permission to access its cookie encryption key.
Chrome, Chromium, Brave, Edge, Firefox, Arc, and Vivaldi are supported. The
currently active Chrome profile is tried first.

While running in an interactive terminal, `unpin` shows Pinterest acquisition
status followed by a progress bar for image downloads and analysis. Progress is
written to stderr, so `--format json` remains clean on stdout.

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

# Use a signed-in browser session when Pinterest truncates anonymous results
unpin https://www.pinterest.com/username/board-name/ --cookies-from-browser chrome
```

Run `unpin --help` for the complete interface.

## How matching works

- Pinterest board metadata is fetched page by page, including board sections.
  Pins repeated in the main feed and a section are counted once.
- When Pinterest's reported total is larger than the number returned by its web
  API, text, JSON, and HTML output show both counts and include an incomplete
  scan warning.
- Ordinary, single-image pins are downloaded with an eight-request concurrency
  limit and a 100 MiB per-image safety limit.
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

- Anonymous scans work with public boards. Signed-in scans can use an explicitly
  selected local browser session; browser cookie values are never persisted.
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
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Tests use local mock HTTP servers and do not contact Pinterest. A live smoke
test can be added separately and should remain opt-in.
