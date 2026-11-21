\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCScoped ----
\* A scoped effect s on a, its dependent d, and a writer w of d on a whose body keeps d alive.
EXTENDS Lifetimes
MCParents == <<{}, {1}, {2}, {1}>>
MCTargets == <<{}, {}, {}, {3}>>
MCCreator == <<0, 0, 0, 0>>
MCHold0 == <<<<0, 1, 0, 0>>, <<1, 0, 0, 0>>>>
MCLeased0 == <<<<0, 0, 0, 0>>, <<0, 0, 0, 0>>>>
====
