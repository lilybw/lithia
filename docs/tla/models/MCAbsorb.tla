\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCAbsorb ----
\* Two writers in their own scopes, each writing one parent of a join: a run started for one write
\* reads the other's before that write's epoch reaches it, which then waits for the run's item.
EXTENDS Scheduling
MCParents == <<{}, {}, {1, 2}, {3}>>
MCTargets == <<{}, {}, {}, {}>>
MCBatches == <<{1}, {2}>>
====
