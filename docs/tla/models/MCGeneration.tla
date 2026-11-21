\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCGeneration ----
\* An effect c on a creates m on a in its run; a top-level effect u on m is created, and c's
\* next run releases m.
EXTENDS Lifetimes
MCParents == <<{}, {1}, {1}, {3}>>
MCTargets == <<{}, {}, {}, {}>>
MCCreator == <<0, 0, 2, 0>>
MCHold0 == <<<<1, 0, 0, 0>>, <<0, 0, 0, 0>>>>
MCLeased0 == <<<<0, 0, 0, 0>>, <<0, 0, 0, 0>>>>
====
