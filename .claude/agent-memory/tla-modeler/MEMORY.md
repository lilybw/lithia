<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# tla-modeler memory

## Running TLC here
- `tlc.sh` gives Java 75% of memory: two TLC processes at once on a 7 GB container got one OOM-killed.
  Run one at a time, `--workers 1` when another job shares the machine.
- Local times on two shared cores: MCWhole about 1 h 50 min, MCDisposal about 10 min, MCKeyed about
  4 min, MCAsyncChain about 10 min, the 2026-11-27 models 3 s to 3 min. MCSharedTarget (8.7 million
  states) belongs on CI; MCDiamond never finishes (26 million states in 4 h in CI) and is bounded.
- MCAbsorb and MCSupersede both have 265 525 states at depth 77. Not a mix-up: both were rerun.

## Modelling lessons
- New features gate behind constants that are off in every existing .cfg; old counts then hold exactly.
- An undone rule that fails nothing means the model cannot reach it: MCKeyed needed its keyed node to
  depend on the creation edge alone; body reflows needed `reflowAt` set in T_Poll before ReflowRan saw them.
- ResilienceConsistent must allow transient windows (a failure, or a disposal's ghosts, not yet
  delivered): see FailUndelivered in Scheduling.tla.
- Engine bugs TLC found, each now a kept mutation: W5 a failure after a release told no child; W6 an
  absorbed change settled when its run was handed over (join_run, queue_whole); W7 framing waited on a
  ghost parent; W8 a run dropped at shutdown told no child. Also: a node disposed while being created
  kept support's self-reference (create.rs).
