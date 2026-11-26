\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCGenerator ----
\* A generator on a source, under two writes: an item keeps the run's epochs open until it ends,
\* and a yield ends a replacement run's protection.
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}, {1}>>
====
