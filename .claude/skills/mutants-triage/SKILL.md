---
name: mutants-triage
description: How to review mutants cargo-mutants leaves alive in lithia - getting the list, telling test gaps from equivalent mutants, writing killing tests, excluding with reasons in .cargo/mutants.toml, verifying kills. Use after a cargo-mutants run or for one file's mutants.
---

# Triaging cargo-mutants survivors

## Getting the survivors

- From CI: the weekly or a manual `extended.yml` run shards cargo-mutants 16 ways. Artifacts may not
  download here, so read the logs: `gh api repos/OWNER/REPO/actions/runs/RUN/jobs?per_page=100` for the
  job ids, then `gh api repos/OWNER/REPO/actions/jobs/ID/logs | grep -E "MISSED|TIMEOUT"`.
- Locally, for a few files: `python3 .github/scripts/mutants.py FILE... [--re REGEX] [-- TEST ARGS]`
  runs in place on the warm build and puts the files back. `--list` shows the mutants without running.
- Survivors found on an older commit: map them by function and replacement text, since lines move.

## Deciding each one

| Class | Action |
| --- | --- |
| A real test gap | A behavioural test in the fitting place: the edge module for the area, a unit test, or a validation case if the spec states the behaviour. No tests that only restate the code. |
| Equivalent: cannot change what lithia does (pure performance, debug-only checks, hints that only skip work, hash quality) | Read the code until sure, then exclude it. |
| Built only under `--cfg lithia_loom` | Exclude; the loom models cover it. |
| Timed out | Counts as caught, unless the timeout hides a missing assertion. |
| Unviable | Nothing to do. |
| A bug | Fix it with a test that fails without the fix, and flag it. |

Public formatting (`Debug`, `Display`, `Error::source` of public types) is API: test it.

## Excluding

`.cargo/mutants.toml` groups `exclude_re` patterns under a comment giving the reason. One pattern per
mutant, anchored with `$`, naming the function and replacement; add `line:column` only where the same
text repeats in a file, so an edit that moves it lets the mutant be tested again rather than hiding a new
one. Never exclude a mutant a test catches.

## Verifying

Every "caught" claim is checked on the mutant itself: `mutants.py FILE --re 'REGEX' -- --test edge`
(a narrower test target is faster). Never commit a mutated file: `git diff` before every commit. After
merging, rerun the files on CI: `gh workflow run extended.yml --ref BRANCH -f mutants="FILE..."`.

## Recording

Update the cargo-mutants paragraph of `docs/known_bugs.md` (what survives and why) and the counts in
`docs/VERIFICATION.md`'s cargo-mutants section, and add a CHANGELOG row (found by "cargo-mutants").
