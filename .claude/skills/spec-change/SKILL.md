---
name: spec-change
description: How to change lithia's design spec and changelog - finding the section, the CHANGELOG row format, the date line, reprinting the PDF. Use for any behaviour or API change, or when the docs must catch up with the code.
---

# Changing the spec

The spec is `docs/spec/LITHIA_design_spec_v<version>.md`, with its PDF beside it. Until 1.0 a new
version replaces the file (git history keeps the old one); from 1.0 each released version keeps its own.

1. Find the section: `grep -n "^## \|^### " docs/spec/LITHIA_design_spec_v*.md`. §3 declares the public
   API; keep it in step with the code (`python3 .claude/tools/outline.py show crates/lithia/src --pub`).
2. Write what the code does now, in plain language and the spec's voice. No history in the body: what
   changed and why goes in the changelog. Cross-reference sections as `(7.4)`.
3. Set the date line (line 3, `YYYY-MM-DD · Lily`) to today.
4. Add a row to the open version's table in `CHANGELOG.md`:
   `| section(s) | what changed, plainly, with the reason | found by |`, where "found by" is review,
   TLA+, loom, cargo-mutants, a test, a user report, and so on.
5. Reprint the PDF: `python3 .github/scripts/spec_pdf.py` (pandoc and Playwright's Chromium), then
   check the new text is in it: `pdftotext docs/spec/LITHIA_design_spec_v*.pdf - | grep -c "PHRASE"`.
6. If the README's example or claims are affected, update them; the example is a doctest
   (`cargo test -p lithia --doc`).

A behaviour change also gets a validation case in `crates/validation` that pins it down, and the
spec's §13 table lists it.
