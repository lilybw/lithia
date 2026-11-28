\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCWhole ----
\* A batches = WHOLE join of a source and an async node on it, under two writes: an epoch that
\* becomes ready while the other still waits is deferred, and the run takes both.
EXTENDS Scheduling
MCParents == <<{}, {1}, {1, 2}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}, {1}>>
====
