\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCCreateRun ----
\* A node created by its creator's run and reading the creator, under two writes: it is
\* seeded in the epochs the run serves, waits on the creator there, and starts once decided.
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}, {1}>>
MCCreatedBy == {<<2, 3>>}
====
