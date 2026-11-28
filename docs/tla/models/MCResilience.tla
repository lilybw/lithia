\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCResilience ----
\* An async node that may panic at any poll, under two writes; its child fails while it is failed
\* (resilience = NONE), and its grandchild only while both are (PARTIAL). Values never change, so
\* a recovered child is restored without a run.
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}, {2, 3}>>
MCTargets == <<{}, {}, {}, {}>>
MCBatches == <<{1}, {1}>>
====
