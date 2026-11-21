\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCSharedTarget ----
\* Two runs serving one epoch both write the same source, whose region is a chain.
EXTENDS Scheduling
MCParents == <<{}, {1}, {1}, {}, {4}, {5}>>
MCTargets == <<{}, {4}, {4}, {}, {}, {}>>
MCBatches == <<{1}>>
====
