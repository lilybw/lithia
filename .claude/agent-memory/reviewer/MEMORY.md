<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# reviewer memory

Invariants changes have broken before, worth checking first:
- Run tasks drop every strong reference to their node before closing epoch tokens; otherwise a settled
  scope can see a node lithia keeps alive (found stress-testing Case J).
- Diagnostics raised under a scheduling lock are gathered and emitted after it is released (spec §11.4).
- A node owes each child at most one release per epoch, and a child counts the releases each parent
  owes it; a release reaches exactly the children its framing counted.
- Rises from zero and writer registration take the sweep lock with their walks; Lifetimes.tla found
  five faults around them.
- Loom models must not hold a std lock across anything loom can switch on.
- New tests must not order by sleeps; see the verifier's memory for the flakes that caused.
