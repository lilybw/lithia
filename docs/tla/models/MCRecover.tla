\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCRecover ----
\* An async node whose value changes and that may panic at any poll, under two writes: its child
\* (resilience = NONE) fails while it is failed, and reruns once it recovers with a new value.
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}, {1}>>
====
