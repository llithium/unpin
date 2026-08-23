---
name: unpin
description: "Cinematic comparison workspace for reviewing duplicate Pinterest image pins."
colors:
  canvas: "#080808"
  shell: "#0b0b0b"
  near-black: "#090909"
  surface: "#111111"
  comparison-surface: "#101010"
  surface-raised: "#181818"
  surface-hover: "#202020"
  image-black: "#070707"
  line: "#292929"
  line-strong: "#494949"
  chrome-line: "#393939"
  comparison-line: "#444444"
  paper: "#f2f0eb"
  muted: "#898783"
  muted-strong: "#c3c0b9"
  signal-red: "#ff3b30"
  white: "#ffffff"
typography:
  display:
    fontFamily: 'Geist, "Helvetica Neue", Arial, sans-serif'
    fontSize: "clamp(52px, 7vw, 112px)"
    fontWeight: 400
    lineHeight: "0.88"
    letterSpacing: "-0.075em"
  headline:
    fontFamily: 'Geist, "Helvetica Neue", Arial, sans-serif'
    fontSize: "clamp(38px, 4vw, 64px)"
    fontWeight: 400
    lineHeight: "0.9"
    letterSpacing: "-0.075em"
  title:
    fontFamily: 'Geist, "Helvetica Neue", Arial, sans-serif'
    fontSize: "clamp(32px, 2.6vw, 48px)"
    fontWeight: 500
    lineHeight: "0.92"
    letterSpacing: "-0.07em"
  body:
    fontFamily: 'Geist, "Helvetica Neue", Arial, sans-serif'
    fontSize: "15px"
    fontWeight: 400
    lineHeight: "18px"
  label:
    fontFamily: 'Geist Mono, ui-monospace, monospace'
    fontSize: "12px"
    fontWeight: 700
    lineHeight: "16px"
    letterSpacing: "0.08em"
rounded:
  square: "0"
  circle: "50%"
spacing:
  hairline: "1px"
  micro: "4px"
  control: "8px"
  compact: "12px"
  field: "16px"
  card: "24px"
  section: "64px"
  content-gutter: "clamp(24px, 6vw, 96px)"
  content-top: "clamp(52px, 7vw, 112px)"
components:
  control:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.paper}"
    typography: "{typography.label}"
    rounded: "{rounded.square}"
    padding: "8px 12px"
    height: "36px"
  primary-action:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.canvas}"
    typography: "{typography.label}"
    rounded: "{rounded.square}"
    padding: "8px 12px"
    height: "36px"
  review-action:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.canvas}"
    typography: "{typography.label}"
    rounded: "{rounded.square}"
    padding: "12px 18px"
    height: "48px"
  scope-filter-selected:
    backgroundColor: "{colors.signal-red}"
    textColor: "{colors.white}"
    typography: "{typography.label}"
    rounded: "{rounded.square}"
    padding: "14px 4px"
  comparison-card:
    backgroundColor: "{colors.comparison-surface}"
    textColor: "{colors.paper}"
    rounded: "{rounded.square}"
    padding: "22px"
  match-nav-active:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.canvas}"
    rounded: "{rounded.square}"
    padding: "18px 20px"
---

# Design System: unpin

## Overview

**Creative North Star: "The Evidence Desk"**

The report is a cinematic, severe workspace for looking closely before acting. It uses a full-bleed ink field, a dark editorial rail, and oversized compressed headlines to make each comparison feel like a piece of evidence brought forward for inspection. The page is intentionally spare: the images, dimensions, board context, and review state carry the visual weight.

The final effective system is the Editorial review system defined at the end of templates/report.html. Its square silhouettes, hairline seams, paper-white selections, and signal-red state changes are authoritative. The earlier rounded workspace preset in the same file is an implementation ancestor, not a direction to revive. Motion is slow and controlled, helping the user move through a sequence of matches without making the review feel playful.

**Key Characteristics:**

- Full-bleed ink-black canvas with a nearly black rail and chrome.
- Severe editorial typography: oversized, tightly tracked Geist headlines paired with compact Geist Mono labels.
- Paper-white primary and selected states; signal red reserved for status, focus, and review signals.
- Square controls and comparison frames joined by one-pixel seams rather than soft elevation.
- Image-first comparison surfaces with restrained interaction and explicit review state.

## Colors

The palette behaves like marked-up evidence: ink establishes concentration, paper marks the active decision, and signal red identifies a state that deserves attention.

### Primary

- **Signal Red:** The scarce action color for active filters, focus rings, progress, review completion, editorial markers, and alert-like scope cues.

### Neutral

- **Canvas Black:** The page field and image surround; it gives the report its deepest contrast.
- **Shell Black:** The structural background for the rail and workspace.
- **Near Black:** The slightly lifted rail and topbar treatment.
- **Surface Black:** The default control and secondary-surface layer.
- **Comparison Black:** The card body tone inside the one-pixel comparison frame.
- **Raised Black:** The badge, stat, and supporting-surface layer.
- **Hover Black:** The small lift used on controls and interactive rows.
- **Image Black:** The stage behind remote images.
- **Line:** The quiet divider used for navigation and section seams.
- **Strong Line:** The visible border used to frame controls and focusable surfaces.
- **Chrome Line:** The stronger rail, topbar, and stat-grid seam.
- **Comparison Line:** The frame and gutter around side-by-side image cards.
- **Paper:** The warm off-white used for primary actions, selected navigation, and dominant type.
- **Muted:** The low-contrast explanatory and metadata voice.
- **Muted Strong:** The readable secondary text and supporting labels.
- **White:** The high-contrast text used on signal-red states.

### Named Rules

**The Signal Red Rule.** Red is a status instrument, not a decorative accent: reserve it for focus, active filters, review states, progress, and editorial markers.

**The Paper-on-Ink Rule.** Use paper as the decisive selected or primary state against the ink field; do not dilute it into a general-purpose surface fill.

## Typography

**Display Font:** Geist (with Helvetica Neue and Arial fallbacks)

**Body Font:** Geist (with Helvetica Neue and Arial fallbacks)

**Label/Mono Font:** Geist Mono (with ui-monospace and monospace fallbacks)

**Character:** The pairing is severe and editorial. Geist gives the report a compressed, modern display voice, while Geist Mono turns IDs, scopes, dimensions, and small labels into measured evidence markers. The implementation does not load a remote font, so the local/system fallback stack is part of the resilient design.

### Hierarchy

- **Display** (400, clamp(52px, 7vw, 112px), 0.88 line-height, -0.075em tracking): Overview-mode match titles; large enough to make the current comparison feel like the page’s subject.
- **Headline** (400, clamp(38px, 4vw, 64px), 0.9 line-height, -0.075em tracking): Focus-mode match titles when the rail and controls need more room.
- **Title** (500, clamp(32px, 2.6vw, 48px), 0.92 line-height, -0.07em tracking): The report identity in the rail.
- **Body** (400, 15px, 18px line-height in focused review): Match descriptions and explanatory copy; keep supporting paragraphs short and operational.
- **Label** (700, 12px, 16px line-height, 0.08em tracking, uppercase): Kicker labels, scope markers, status badges, and control text.

### Named Rules

**The Compression Rule.** Large headings should feel cut from one editorial block: tight tracking and short line-height create authority without adding ornament.

**The Mono Evidence Rule.** Put IDs, dimensions, percentages, keyboard hints, and other inspectable values in Geist Mono so evidence reads differently from explanation.

## Layout

The desktop composition is a full-bleed workspace with a sticky 78px topbar and a left rail that occupies 25vw with a 300px minimum. The rail owns scan context, statistics, filters, match navigation, and shortcuts; the content field owns the current comparison. The content uses responsive side gutters of clamp(24px, 6vw, 96px) and a top offset of clamp(52px, 7vw, 112px), with match content capped at 1440px.

Comparison pairs are deliberately image-first. In focus view, the image stage fills the available viewport height between 260px and 720px, while the card body compresses to 10px 16px so dimensions and actions stay attached to the evidence. In overview mode, each match becomes a framed, sticky evidence panel with generous internal padding and a long vertical rhythm between panels.

At 880px, the rail becomes a horizontal match strip and the workspace becomes one column; the report keeps the topbar and moves the content into a 56px top rhythm. At 620px, the topbar tightens to 64px, the brand wordmark collapses to its mark, comparisons become one column, and image stages keep a 300px minimum. Print mode removes the navigation chrome, restores a white field, and breaks each match onto its own page.

## Elevation & Depth

The final system is flat with tonal seams. The app shell and comparison cards do not use shadows; depth comes from near-black layer changes, one-pixel borders, and the one-pixel gutter between cards. The sticky topbar may use a translucent black surface with a 24px blur, but it should still read as a sheet of ink rather than a floating panel.

### Shadow Vocabulary

- **No resting shadow:** The final Editorial review system uses no box shadow for the shell, rail, or comparison cards.
- **Tonal seam:** Use line, strong line, chrome line, or comparison line to define structure before reaching for any lift.

### Named Rules

**The Flat-Seam Rule.** If a surface can be separated with a tonal change or one-pixel rule, do that before adding a shadow.

## Shapes

The form language is square and editorial. Controls, badges, filter segments, navigation rows, comparison cards, and image frames use a zero-radius silhouette. The only recurring circular shapes are the brand mark and the tiny review-complete dot; they function as marks, not containers. The progress track and scope markers are also square in the final override.

Borders are hairline seams rather than decorative outlines. Use strong lines for interactive affordances, chrome lines for structural divisions, and comparison lines to create the shared frame around side-by-side cards. Avoid pills, inflated rounded cards, and soft floating islands.

## Components

### Buttons

- **Character:** Editorial instruments: quiet at rest, decisive in the active state.
- **Shape:** Square corners (0px), uppercase Geist Mono labels with 0.08em tracking.
- **Default control:** Surface-black fill, strong-line border, paper text, 36px minimum height, and 8px 12px padding.
- **Primary action:** Paper fill with canvas text; the final report uses this for opening a pin or image.
- **Review action:** Paper fill at 48px minimum height with 12px 18px padding; the completed state turns signal red.
- **Hover / Focus:** Hover moves the surface to signal red for primary/review actions; focus-visible uses a 2px signal-red outline with a 2px offset.
- **Pressed / Disabled:** Pressed controls scale to 0.98; disabled controls keep their structure but drop to 0.38 opacity and show a not-allowed cursor.

### Chips

- **Style:** Status badges use zero-radius silhouettes, a raised-black background, Geist Mono, uppercase lettering, and 0.08em tracking.
- **State:** Signal red marks cross-board or attention states; paper marks selected or positive states; muted strong marks neutral scope.
- **Board labels:** Board names use a compact raised-black block with 4px 8px padding and a 4px corner only where the content needs a small label container.

### Cards / Containers

- **Corner Style:** Square in the final system (0px).
- **Background:** Comparison cards use comparison black; their image stages use image black.
- **Shadow Strategy:** No resting shadow; use the shared comparison frame and tonal seams.
- **Border:** The comparison grid supplies a one-pixel comparison-line frame and one-pixel gutters.
- **Internal Padding:** Focus cards use 10px 16px for compact evidence metadata; overview cards use responsive 36px to 72px framing.

### Inputs / Fields

- **Style:** The scope filter uses visually hidden radio inputs paired with square segmented labels. Each label has 14px vertical padding and a one-pixel right seam.
- **Focus:** The selected label receives a signal-red fill and white text; keyboard focus receives a 2px signal-red outline with a 2px offset.
- **Error / Disabled:** No custom error field is present. Disabled controls use the shared 0.38 opacity treatment.

### Navigation

- **Desktop:** A sticky rail owns the scan identity, statistics, filter segments, match list, and keyboard shortcuts. Match rows are square, border-separated, and padded 18px 20px.
- **Active:** The selected row becomes paper with canvas text; its match index returns to signal red and its metadata drops to a darker neutral.
- **Hover:** The row shifts its left padding by 6px without vertical lift.
- **Responsive:** At 880px the rail becomes a horizontally scrollable strip of 190px match items; the intro, statistics, and shortcuts recede.

### Comparison Workbench

Each match is a focused evidence frame: a Mono kicker, an oversized match title, a short operational description, a review action, and a side-by-side comparison. Images remain unfiltered and centered; the only image motion is a slow opacity reveal as remote assets become ready. The overview toggle exposes the entire sequence, while focus view keeps one match active and pre-warms its neighbors.

## Do's and Don'ts

### Do:

- **Do** keep the visual hierarchy severe: large compressed titles, short descriptions, and images before decoration.
- **Do** use signal red only where the user needs to notice a state, not as a general brand wash.
- **Do** use paper-on-ink for primary and selected states.
- **Do** preserve square silhouettes and one-pixel seams across controls, navigation, filters, and comparison frames.
- **Do** use Geist Mono for inspectable data and keep focus-visible outlines explicit.
- **Do** honor prefers-reduced-motion by collapsing transitions to near-zero duration.

### Don't:

- **Don't** reintroduce the earlier rounded-card, pill, or ambient-shadow preset into the final editorial system.
- **Don't** add decorative gradients, glows, image filters, or parallax to the comparison surface.
- **Don't** make every control red; red is a signal and loses meaning when it becomes a background theme.
- **Don't** let supporting copy compete with the images or the match heading.
- **Don't** hide keyboard focus behind the cinematic treatment.
