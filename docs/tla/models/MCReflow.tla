\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCReflow ----
\* A write to a source and a reflow of the async node below it: the node reruns, a run already
\* under way is waited for, and the child sees the reflow as a change.
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}, {2}>>
====
