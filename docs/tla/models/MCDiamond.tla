\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: GPL-2.0-or-later
---- MODULE MCDiamond ----
\* Two outside writes into a diamond: glitch freedom, supersession and handover of sync runs.
EXTENDS Scheduling
MCParents == <<{}, {1}, {1}, {2, 3}>>
MCTargets == <<{}, {}, {}, {}>>
MCBatches == <<{1}, {1}>>
====
