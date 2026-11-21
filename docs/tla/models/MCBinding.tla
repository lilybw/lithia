\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCBinding ----
\* A two-way binding of two sources: a write cycle through two writers.
EXTENDS Scheduling
MCParents == <<{}, {}, {1}, {2}>>
MCTargets == <<{}, {}, {2}, {1}>>
MCBatches == <<{1}>>
====
