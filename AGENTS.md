# Codex project instructions

For complex coding tasks, use the `astra-orchestrator` skill when its trigger conditions match.

The root agent owns architecture, decomposition, integration, and final verification.
Prefer specialized subagents for bounded exploration, implementation, testing, review, and technical research.

Route exploration, research, and testing to the named Luna roles at high
reasoning; route bounded implementation and batch edits to the Luna worker at
max reasoning. Route complex cross-file implementation and difficult debugging
to the Sol `solver` role. Keep the Sol root focused on architecture,
coordination, integration, and final acceptance, with Astra focused on
independent review.

For delegated work expected to run longer than 15 minutes, create a 15-minute
heartbeat when the current Codex surface supports recurring thread monitoring.
Use it only to inspect new results, correct drift, or unblock work, and stop it
when all tasks finish. Otherwise prefer event-driven updates over frequent polling.

When recurring monitoring is unavailable, never simulate a heartbeat with busy
polling; prefer completion events or bounded waits. Do not reread thread lists,
logs, or repository status unless state changes or a specific diagnostic need
arises. Record active child IDs and the current phase once. If a later wake-up
is needed, hand the checkpoint to a supervising task in the Windows Codex app;
the remote root must not poll to create its own heartbeat.

Use the smallest useful agent set, usually 1–3 agents and fewer for simple tasks;
parallelize only truly independent tasks. Do not repeat full-repository scans
unless new evidence invalidates the baseline. Subagents should return concise
summaries of changed files, key findings, verification results, and blockers,
rather than raw logs. Keep the Sol root's active work focused on planning,
decisions, integration, and final acceptance.

Do not delegate trivial work merely for parallelism.
Do not let multiple implementation agents edit the same files without explicit ownership boundaries.
User instructions always take precedence over this orchestration policy.

## Configured agent topology

- root: GPT-5.6 Sol via `openai`, high reasoning; owns architecture decomposition, coordination, integration, and final acceptance.
- explorer, tester, researcher: GPT-5.6 Luna via `openai`, high reasoning; handle exploration, validation, and technical research.
- worker: GPT-5.6 Luna via `openai`, max reasoning; handles bounded implementation.
- solver: GPT-5.6 Sol via `openai`, high reasoning; handles complex implementation and difficult debugging.
- reviewer: GPT-6 Astra via `openai`, low reasoning; performs read-only independent review.
- unnamed subagents: GPT-5.6 Luna via `openai`, high reasoning.

All roles retain `sandbox_mode = "danger-full-access"` and the existing
`approval_policy`. On this Windows host, lowering the sandbox causes the
PowerShell startup failure `0xC0000142`, so the sandbox cannot be downgraded.
