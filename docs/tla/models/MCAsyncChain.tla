\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: GPL-2.0-or-later
---- MODULE MCAsyncChain ----
\* Two outside writes into an async node that polls twice: child epochs, protected runs.
EXTENDS Scheduling
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {}>>
MCBatches == <<{1}, {1}>>
====
