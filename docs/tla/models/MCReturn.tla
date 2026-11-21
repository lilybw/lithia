\* SPDX-FileCopyrightText: 2026 lithia Authors
\* SPDX-License-Identifier: MPL-2.0
---- MODULE MCReturn ----
\* Sources a and y, an effect e on a and its child c on e and y: handles move between threads,
\* bodies clone what they captured, and a block's lease ends.
EXTENDS Lifetimes
MCParents == <<{}, {}, {1}, {2, 3}>>
MCTargets == <<{}, {}, {}, {}>>
MCCreator == <<0, 0, 0, 0>>
MCHold0 == <<<<1, 0, 0, 0>>, <<0, 1, 0, 0>>>>
MCLeased0 == <<<<0, 0, 0, 0>>, <<1, 0, 0, 0>>>>
====
