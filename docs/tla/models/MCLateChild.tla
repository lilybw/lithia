\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCLateChild ----
\* Sources a and b, and effects c on a and d on a and b, created while a's disposal is under way:
\* a child linked after the walk, before its parent closed, is disposed of if all its parents are
\* ghosts (c) and survives otherwise (d); one linked after its parent closed disposes of itself.
EXTENDS Lifetimes
MCParents == <<{}, {}, {1}, {1, 2}>>
MCTargets == <<{}, {}, {}, {}>>
MCCreator == <<0, 0, 0, 0>>
MCHold0 == <<<<1, 1, 0, 0>>, <<0, 0, 0, 0>>>>
MCLeased0 == <<<<0, 0, 0, 0>>, <<0, 0, 0, 0>>>>
====
