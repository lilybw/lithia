\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCKeyed ----
\* An effect c on a creates the keyed source m in its run, and a top-level effect u on m is created,
\* which only the creation edge supports; c's next run holds m over, then reclaims it or ends and
\* releases it.
EXTENDS Lifetimes
MCParents == <<{}, {1}, {}, {3}>>
MCTargets == <<{}, {}, {}, {}>>
MCCreator == <<0, 0, 2, 0>>
MCHold0 == <<<<1, 0, 0, 0>>, <<0, 0, 0, 0>>>>
MCLeased0 == <<<<0, 0, 0, 0>>, <<0, 0, 0, 0>>>>
====
