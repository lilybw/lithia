\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCReflowBack ----
\* A child whose body reflows its parent, under a write: the parent reruns on the chain the child
\* wrote on, and the child, cut, reruns once the epoch settles (Case P with a reflow).
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {2}>>
MCBatches == <<{1}>>
====
