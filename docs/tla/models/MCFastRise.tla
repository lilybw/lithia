\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCFastRise ----
\* A source a and an effect e on a that only support holds: a's last handle drops while two threads clone
\* a from e's body, rising from zero without the sweep lock and adding onto that rise before it is done.
EXTENDS Lifetimes
MCParents == <<{}, {1}>>
MCTargets == <<{}, {}>>
MCCreator == <<0, 0>>
MCHold0 == <<<<1, 0>>, <<0, 0>>, <<0, 0>>>>
MCLeased0 == <<<<0, 0>>, <<0, 0>>, <<0, 0>>>>
====
