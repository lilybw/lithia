\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCCreate ----
\* A node created from outside below an async node a write reaches (Case N): it opens
\* entries in the epochs its parent still owes, which wait for its seed, and runs its first run.
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}>>
====
