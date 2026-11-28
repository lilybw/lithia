<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# verifier memory

- Baselines at 842b401 (2026-11-28): 282 workspace tests; 52 loom models, about 2 min with the fast
  path and 3 without; 28 TLC models in counts.tsv.
- Timing flakes show on macOS runners (the extended run's stress job) long before Linux or this machine:
  Case BV's timeout, `defer_outside_lithia_tasks_follows_the_latest_write`, and
  `a_superseded_run_publishes_nothing` before c267107. Check new tests with repeated runs, one core too.
- A test can hang rather than fail when a body waits at a gate the failing test never opens: kill the
  test binary (`pkill -f` its name) instead of waiting for it.
- After reverting a fix to watch its test fail, `git status` must come back exactly as it was.
