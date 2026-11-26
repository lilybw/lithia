\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCDisposal ----
\* Sources a and b, an effect e on both and a child d of e, created later: disposing of b leaves e a
\* surviving child that a alone supports; disposing of e takes d with it, or d is created with only a
\* ghost for a parent and is disposed of at once.
EXTENDS Lifetimes
MCParents == <<{}, {}, {1, 2}, {3}>>
MCTargets == <<{}, {}, {}, {}>>
MCCreator == <<0, 0, 0, 0>>
MCHold0 == <<<<1, 1, 0, 0>>, <<0, 0, 0, 0>>>>
MCLeased0 == <<<<0, 0, 0, 0>>, <<0, 0, 0, 0>>>>
====
