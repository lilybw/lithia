<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# docs-sync memory

- The README's example is a doctest (`ReadmeExample` in crates/lithia/src/lib.rs) since it went stale
  once, still importing `effect` after the rename to `node!`.
- Spec layout: §3 declares the API (check against `outline.py show crates/lithia/src --pub`), §13 lists
  the validation cases, §16 the roadmap (changes that keep API and behaviour, then the two that would
  not), §17 open questions (before 1.0, then additive).
- CHANGELOG rows: `| section | change | found by |`, appended to the open version's table; old rows keep
  old names such as `effect!`.
- `spec_pdf.py` changes the PDF's bytes on every print: reprint only when the text changed.
- The maintainers' progress notes live in a claude.ai project, not in the repository.
