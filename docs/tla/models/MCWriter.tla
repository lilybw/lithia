\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCWriter ----
\* An effect e on source x, kept alive by its child c on e and a; a writer w of x on a is created.
EXTENDS Lifetimes
MCParents == <<{}, {}, {2}, {1, 3}, {1}>>
MCTargets == <<{}, {}, {}, {}, {2}>>
MCCreator == <<0, 0, 0, 0, 0>>
MCHold0 == <<<<1, 0, 0, 0, 0>>, <<0, 1, 0, 0, 0>>>>
MCLeased0 == <<<<0, 0, 0, 0, 0>>, <<0, 0, 0, 0, 0>>>>
====
