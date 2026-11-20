\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: GPL-2.0-or-later
---- MODULE MCWriteBack ----
\* A body writes its own parent (Case P) beside a join: cuts and cut reruns.
EXTENDS Scheduling
MCParents == <<{}, {1}, {1, 2}>>
MCTargets == <<{}, {1}, {}>>
MCBatches == <<{1}>>
====
