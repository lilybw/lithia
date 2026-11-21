\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCAsyncChain ----
\* Two outside writes into an async node that polls twice: child epochs, protected runs.
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}, {1}>>
====
