\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCDefer ----
\* A writer of a source an async node reads defers a function that writes the source again:
\* the function runs once the writer's epoch has settled, and flush waits for it.
EXTENDS Scheduling
MCParents == <<{}, {1}>>
MCTargets == <<{}, {}>>
MCBatches == <<{1}, {1}>>
MCDeferredBy == {<<1, 2>>}
====
