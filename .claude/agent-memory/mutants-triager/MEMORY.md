<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# mutants-triager memory

## Running mutants
- Locally: `python3 .github/scripts/mutants.py FILE --re REGEX -- --test edge`; one mutant with a warm
  build and `-- --lib` takes about 30 s.
- On CI: `gh workflow run extended.yml --ref BRANCH -f mutants="FILES" -f mutants_re=REGEX`. The weekly
  run shards 16 ways with cargo-mutants 27.1.0. Artifacts don't download here: grep job logs for MISSED.

## The review of 2026-11-27 (run 45681040261 on ec53abb, mapped onto 6041af0)
- 518 targets: 123 caught after new tests, 33 time out, 33 unviable, 169 excluded in .cargo/mutants.toml,
  160 alive (docs/known_bugs.md). Of 11 alive protocol mutants sampled, 5 fail a loom model at two
  preemptions: cargo-mutants does not run loom, so many survivors are interleaving-only paths.
- One real bug: a `&'static` handle used as a `move` body's dependency was taken as moved in (93af099).
- In `Node::publish`, the run-id check is redundant with `run.is_cancelled()` for supersession (a
  supersede cancels): removing it fails nothing; removing the cancel check fails
  `a_superseded_run_publishes_nothing`.

## Tests that hold up
- Don't order with sleeps: `a_superseded_run_publishes_nothing` first slept and failed 57 of 400 macOS
  stress runs, because a write is delivered after `set` returns. Wait for the effect instead, e.g. the
  trace `lithia_test::find(Level::TRACE, &["run superseded or aborted", "file.rs:LINE"], limit)`
  after `lithia_test::events()` installs the collector.
- To hold a body, `tokio::task::block_in_place(|| while !gate.load(..) { sleep(1 ms) })` on a
  multi_thread runtime, and open every gate before asserting: otherwise a failing test hangs in the
  runtime's shutdown instead of failing.
- Before committing a timing-sensitive test: 200 runs, and 100 more under `taskset -c 0`.
