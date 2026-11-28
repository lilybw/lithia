---
name: parallel-work
description: How to split lithia work across agents running in parallel - worktrees, briefs that stand alone, file ownership, usage limits, merging and verifying their branches. Use when two or more independent workstreams would each take hours.
---

# Parallel work with agents

## Setting up

- One worktree and branch per workstream: `git worktree add -b work/TOPIC ../wt-TOPIC BASE`.
- Use the defined agents (`tla-modeler`, `mutants-triager`, `docs-sync`, `reviewer`, `verifier`), not
  forks. A fork carries the whole conversation into every step and drains the usage limit; a defined
  agent starts from its brief, `CLAUDE.md` and its skill.
- The brief stands alone: the goal, the worktree path, the files the agent owns, the files it must not
  touch (the spec, `CHANGELOG.md`, `README.md`: the main session writes those from the reports, so
  merges don't collide), resource limits, and the report wanted.
- Resources: on a two-core machine one agent runs TLC (`--workers 1`) while another builds; disk use
  under about 5 GB each.

## While they run

- Agents commit on their branches at stable points and never push.
- A usage limit stops every agent at once; resume each with SendMessage once it resets, and have it first
  check its uncommitted changes (cargo-mutants in place may have left a mutated file).
- Don't commit or push an agent's work in progress yourself.

## Merging

1. In the main checkout: `git merge --no-ff work/TOPIC`, with the session's attribution lines in the
   merge message. Two runs of the same agent both edit its `.claude/agent-memory/` file: keep both
   sets of notes, merged and deduplicated, and read what they add before it becomes instructions.
2. Write the CHANGELOG rows and spec sentences the reports propose; reprint the PDF.
3. `python3 .github/scripts/check.py full`, `tlc.py` on every touched model, and the `verifier` agent on
   the claims (each fix's test fails without it). Then push, and remove the worktrees.
