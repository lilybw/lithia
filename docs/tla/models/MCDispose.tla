\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCDispose ----
\* Disposing of a node a write reaches: its run is torn down, the child it fails
\* (disposed = FAIL, resilience = NONE) fails, and the other survivor reruns.
EXTENDS Scheduling
MCParents == <<{}, {1}, {1, 2}, {1, 2}>>
MCTargets == <<{}, {}, {}, {}>>
MCBatches == <<{1}>>
====
