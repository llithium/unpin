# Glossary

## Scan source

A board feed or profile feed whose pins contribute to a Scan. A Scan can combine multiple board sources and the Unorganized ideas source.

## Scan intake

The part of a Scan that resolves its target and selected sources, collects their pins, and preserves source-level outcomes before image analysis.

## Scan

A single run of `unpin` that gathers pins from the requested Pinterest target
and analyzes the gathered static images for duplicate candidates.

## Progress step

A named stage of a scan that can be active while work is happening and
complete once that work has finished. Completed steps remain part of the scan
history so the final state explains what the run did.
