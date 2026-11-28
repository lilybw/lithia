---
name: release
description: How work moves from a verify branch to nightly and on to crates.io in lithia, what must be green first, and what only the maintainer does (tags, crates.io). Use only when the maintainer asks for a merge to nightly, a release, or a version bump.
disable-model-invocation: true
---

# Releasing lithia

Only on the maintainer's explicit request. Never push tags (the maintainer tags), never delete remote
branches, never publish to crates.io without being handed the token for it.

## Gates, on the exact commit

- `ci.yml` green: tests on Linux x86-64 and ARM, macOS, Windows and the minimum rustc; loom at bound 3
  both ways; TLC on every `ci.yml` model with its recorded count.
- `extended.yml` green: Miri, loom at bound 4 both ways, stress on three platforms, TLC on the large
  models (`MCDiamond` bounded), the kept mutations.
- The latest cargo-mutants run reviewed: nothing alive that `known_bugs.md` does not explain.

## Flow

1. `verify/...` → `nightly`, by pull request or, when the maintainer says so, a direct push. Each push to
   `nightly` is released as `v<version>.<n>` by `nightly.yml`, and the docs site is rebuilt.
2. `nightly`, the default branch, → crates.io: a nightly commit on which the extended checks passed too
   is a release candidate, published with `publish.yml` (below).

## Publishing

`publish.yml`, run by the maintainer from `nightly` (CONTRIBUTING.md, Publishing to crates.io). Before
asking for it: the version bump below committed on `nightly`, its `nightly` run green, and a full
`extended` run dispatched on that commit and green. Never run it with `dry-run` off yourself.

## A version bump

- `version` in the workspace `Cargo.toml`, and `lithia`'s dependency on `lithia-macro`.
- `CHANGELOG.md`: the open heading becomes `## X.Y.Z (YYYY-MM-DD)` with a one-paragraph summary.
- The spec file's name and title, then `python3 .github/scripts/spec_pdf.py`.
- README's install line, if it names a version.
