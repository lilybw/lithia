\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCCreator ----
\* A source read by a creator and by the node its run created, under two writes: the created
\* node waits while its creator may still replace it, and starts once the creator moved on.
EXTENDS Scheduling
MCParents == <<{}, {1}, {1}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}, {1}>>
MCCreatedBy == {<<2, 3>>}
====
