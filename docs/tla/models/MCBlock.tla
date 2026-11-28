\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCBlock ----
\* An async source block, created from outside, and its child, under a write: the write stops
\* the block, before it starts or while it runs, and its late items are discarded.
EXTENDS Scheduling
MCParents == <<{}, {1}>>
MCTargets == <<{}, {}>>
MCBatches == <<{1}>>
====
