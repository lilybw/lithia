---
name: tla-models
description: How to extend or check lithia's TLA+ specs and TLC models in docs/tla - gating features behind constants, keeping state counts, mutation tables, turning a violation into an engine fix. Use for any work on docs/tla or on scheduling or lifetime code that the specs model.
---

# Working on lithia's TLA+ models

`docs/tla/README.md` is the map: what each spec models at which grain, what is left out, every
property, every model with its state count, and the mutation tables. Read it first. Outline the specs
with `python3 .claude/tools/outline.py show docs/tla` (operators and their comments, no bodies).

## Adding a feature to a spec

1. Model it at the engine's grain: one step per atomic operation or lock section, named after the code
   (`outline.py show crates/lithia/src/protocol` and the like give the functions).
2. Gate it behind new constants that are off by default (`{}` or `FALSE`), and add them to every model's
   `.cfg`, so every existing model explores exactly the states it did. `python3 .github/scripts/tlc.py
   MODEL...` compares with `docs/tla/models/counts.tsv`; a moved count means the gate leaks.
3. State what the feature promises as properties (invariants, or temporal properties under fairness),
   each with a row in the README's table.
4. Add a model `MCName.tla`/`.cfg` that turns it on, small enough to finish: under about 20 minutes on
   two workers goes in `ci.yml`'s TLC matrix, longer ones in `extended.yml`. Record it with
   `tlc.py MCName --record`.
5. Undo each new rule in the spec, one at a time, and confirm a property fails; record the rule, the
   property, the model and the time in the README's mutation table, then put the rule back. A rule
   whose removal fails nothing is either redundant or under-checked: shrink or reshape the model until
   it fails.

## When TLC finds a violation

- The trace is in `target/tlc/MODEL.log`. Map each step to the engine's code and decide: a model error
  (the spec does something the engine cannot), or an engine bug.
- An engine bug gets a test that fails without the fix: an edge test in `crates/test/tests/edge`, or a
  loom model in `crates/test/tests/loom` for interleavings. Revert the fix and watch the test fail.
- Add a kept mutation to `.github/scripts/mutations.py` that undoes the fix, with the test that catches
  it; `.github/scripts/mutations.py ID` must report it caught.
- Fix the model's counterpart too, and rerun every model whose count could move.

## Resources

TLC's fingerprint set takes a quarter of memory per process: run one TLC at a time, with
`--workers 1` when another job shares the machine. Hour-long models go to CI:
`gh workflow run extended.yml --ref BRANCH -f tlc="MCName"`.

## Afterwards

Update `docs/tla/README.md` (modelled, left out, properties, models, mutation tables), the TLA+
sentence of spec §11.4, a CHANGELOG row (found by "TLA+"), and `counts.tsv` with the reason in the
commit message for any count that moved.
