\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: GPL-2.0-or-later
---- MODULE MCSupersede ----
\* Two outside writes into a chain of synchronous nodes: supersession and handover.
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}, {1}>>
====
