\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCSupersede ----
\* Two outside writes into a chain of synchronous nodes: supersession and handover.
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}, {1}>>
====
