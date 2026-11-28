\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCSkip ----
\* A source a, an effect p on a and an effect m on p, both held by support: a's and m's last handles
\* drop on two threads, so m's fall may skip its sweep while a's sweep lets go of p.
EXTENDS Lifetimes
MCParents == <<{}, {1}, {2}>>
MCTargets == <<{}, {}, {}>>
MCCreator == <<0, 0, 0>>
MCHold0 == <<<<1, 0, 0>>, <<0, 1, 1>>>>
MCLeased0 == <<<<0, 0, 0>>, <<0, 0, 0>>>>
====
