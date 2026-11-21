<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# TLA+ model of the scheduling protocol

[`Scheduling.tla`](Scheduling.tla) specifies lithia's scheduling protocol (design spec §7.2–7.4, §11.4)
at the grain of the engine's lock sections, and TLC checks it on the small graphs in [`models/`](models).
It is the TLA+ specification §11.4 step 8 asks for, for the scheduling half; node lifetimes are not in it
yet (see below).

## What it models

Each process step is one lock section or one atomic operation of `protocol.rs`, `run.rs` or `epoch.rs`:

- outside writers, each delivering one batch as a root epoch;
- run tasks (`Slots` per node): starting a run, polling its body (which may write its targets, change its
  value or not, and suspend to poll again), the poll's delivery, releasing or handing over its epochs,
  supersession, protection of a replacement run, and the fast path's task boundaries;
- cut reruns, started when their epoch settles;
- framing, releases, deciding and settling entries, counted waits, and the epoch tree's token
  accounting.

A process's pending frame visits and releases are sets, handled in every order: a superset of the
engine's. Bodies are abstract: a run changes its value or not, and writes any subset of its targets,
freely while the clock stays below `MaxClock`.

Not modelled: node creation and lifetimes (generations, support, sweeps, leases), creator order,
fragility, reflow, generators, `WHOLE_EPOCHS` and disposal. The loom models and the edge tests cover
those.

## Properties

| Property | Meaning |
| --- | --- |
| `TokensConserved` | Every open token in an epoch is held by exactly one entry, run, handover or pending close. |
| `TreeCounts` | Each epoch's `sub` is its own count plus its child epochs with open work; the scope counts root epochs with open work. |
| `NoStuckWait`, `WaitsCounted` | A child waits on a parent only for releases that parent still owes it or has under way. |
| `ObligationsHeld` | An unreleased obligation holds a token, so its epoch cannot settle before the release. |
| `ScopeConsistent` | When the scope's count is zero (`flush` returns), every node's last run started after its parents' last changes, except changes not yet delivered. |
| `NoGlitch` | Without writes from bodies, no run starts on an entry while a parent still owes the epoch. |
| `Terminates` | Under weak fairness, every behaviour reaches a state with no work left. |

## Models

| Model | Graph and load |
| --- | --- |
| `MCDiamond` | A diamond under two outside writes: glitch freedom, supersession and handover. |
| `MCSupersede` | A chain of synchronous nodes under two outside writes. |
| `MCAsyncChain` | The same with an async middle node that polls twice: child epochs and protected runs. |
| `MCSharedTarget` | Two runs serving one epoch write the same source, whose region is a chain. |
| `MCWriteBack` | A body writes its own parent beside a join (Case P): cuts and cut reruns. |
| `MCBinding` | A two-way binding of two sources: a write cycle through two writers. |

## Running

```sh
curl -LO https://github.com/tlaplus/tlaplus/releases/download/v1.8.0/tla2tools.jar
cd docs/tla/models
java -XX:+UseParallelGC -DTLA-Library=.. -cp ../../../tla2tools.jar tlc2.TLC -workers auto MCDiamond
```

## Findings

TLC's first runs found two races in v0.4.0.1, and the loom model written for the first exposed a third.
Each is reproduced in the engine by a loom model and fixed in v0.4.0.2 (spec §7.2, §7.3, §11.4):

1. Two runs serving one epoch that wrote the same source each released its children, through one wait
   per child. One delivery's release could reach a child while the other still framed the child's
   region, so a node below waited forever and the epoch never settled.
2. A run superseded after publishing its value dropped the value's stamp, so its children never reran
   if the run replacing it computed an equal value. The successor now releases with that stamp.
3. A parent framed again in an epoch while its earlier release there was still under way gave its
   child two releases for one wait; the one carrying no change could arrive first and settle the child,
   which then never saw the change.

The fix for the first and third: a child counts the releases each parent owes it, and each release
reaches exactly the children its framing counted. For the second, the run that takes over a superseded
run's epochs releases with the stamp of the item it published.
