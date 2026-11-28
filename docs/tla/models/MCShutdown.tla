\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCShutdown ----
\* An async node and its child (resilience = NONE) under a write while the runtime
\* shuts down: dropped tasks give their nodes up, which fail, and flush still returns.
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}>>
====
