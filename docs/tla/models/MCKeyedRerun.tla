\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCKeyedRerun ----
\* A keyed node without parents that its creator's runs reclaim, under two writes: it waits until
\* reclaimed, and a reclaim with new captures reruns it, also while a run of its is under way.
EXTENDS Scheduling
MCParents == <<{}, {1}, {}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}, {1}>>
MCCreatedBy == {<<2, 3>>}
====
