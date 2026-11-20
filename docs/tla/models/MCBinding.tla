\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: GPL-2.0-or-later
---- MODULE MCBinding ----
\* A two-way binding of two sources: a write cycle through two writers.
EXTENDS Scheduling
MCParents == <<{}, {}, {1}, {2}>>
MCTargets == <<{}, {}, {2}, {1}>>
MCBatches == <<{1}>>
====
