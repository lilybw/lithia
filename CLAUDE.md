<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# Working on lithia with Claude

lithia is a thread-safe, fine-grained reactive engine for Rust on tokio. The design spec
(`docs/spec/LITHIA_design_spec_v*.md`) defines its behaviour; `CONTRIBUTING.md` has the layout and the
conventions, `docs/VERIFICATION.md` what checks it, `docs/known_bugs.md` what is known to be wrong.

## Start here

- `python3 .claude/tools/session_start.py` prints the branch, the tools present and a map of the code.
  A SessionStart hook runs it where the project's settings load; run it yourself otherwise.
- Read code through outlines before opening files: `python3 .claude/tools/outline.py show FOLDER`
  (or a file; `--pub` for the public API) gives signatures, doc comments and line numbers, cached per
  folder and rebuilt only when a hash of its sources changes. Then Read just the lines you need.
  `outline.py status` names folders changed since their outline was built; `outline.py hash FOLDER`
  lets you tell later whether code you read has changed.

## Checks

- `python3 .github/scripts/check.py` (fmt, clippy, rustdoc, tests, harness tools) before every commit;
  `check.py full` adds clippy and the loom models under `--cfg lithia_loom`, both with and without
  the fast path, for anything touching the engine. One line per step; logs in `target/check/`.
- `python3 .github/scripts/tlc.py MODEL...` runs TLC and compares each model with
  `docs/tla/models/counts.tsv`. A model or spec change that moves a count is recorded with `--record`
  in the same commit, and the commit says why it moved.
- `.github/scripts/mutations.py ID` applies a kept mutation; `python3 .github/scripts/mutants.py FILE
  --re REGEX` runs cargo-mutants in place on a few mutants.
- Long runs go to CI instead of this machine: `gh workflow run extended.yml --ref BRANCH -f tlc="MCA
  MCB"`, or `-f mutants="FILE..." -f mutants_re=REGEX`; follow with `gh run view`. Artifacts may not
  download here; read job logs (`gh api repos/OWNER/REPO/actions/jobs/ID/logs`).

## Rules for changes

- Every binding and closure parameter is typed. Python is typed and passes `mypy --strict`; no
  JavaScript (TypeScript without `any`). Doc comments: at most two lines, the standard library's
  voice, an example for generic APIs; no version history in comments. SPDX header on every file.
- A behaviour change updates the spec section, adds a row to the open version's table in
  `CHANGELOG.md` (`| section | what changed | found by |`) and reprints the PDF with
  `python3 .github/scripts/spec_pdf.py`.
- A bug fix comes with a test that fails without the fix: revert the fix and watch it fail before
  committing. Where it fits, add a kept mutation to `.github/scripts/mutations.py`.
- A change to scheduling or lifetimes updates `docs/tla` too, and every old model keeps its count
  unless the change means it to move.
- Tests must not depend on sleeps for ordering: wait on a condition (see `crates/test/src`).

## Git

- Work on a `verify/...` branch. `nightly` and `release` take only fully verified work (CI and the
  extended run green), and only when the maintainer asks. Never push tags or delete remote branches.
- Commit with the identity configured in the clone; don't change it or print it. Agents commit on
  their own branches and never push; the main session merges, checks and pushes.

## Agents

`.claude/agents/` defines `tla-modeler`, `mutants-triager`, `verifier`, `reviewer` and `docs-sync`.
Prefer them to forks: a fork carries the whole conversation into every step, which drains usage. Give
each a brief that stands alone, its own worktree (`git worktree add`) for parallel work, and the
files it owns, so merges don't collide. Skills in `.claude/skills/` hold the longer procedures
(release, spec changes, TLA+ models, mutant triage, parallel work).

This machine may have only two cores: run one TLC (`--workers 1` when sharing) and one cargo build at
a time, keep command output filtered, and delete large outputs you no longer need.
