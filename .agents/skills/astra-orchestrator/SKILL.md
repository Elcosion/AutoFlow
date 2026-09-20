---
name: astra-orchestrator
description: Orchestrate complex Codex work with Sol as planner and integrator, Luna for routine execution, Sol for difficult implementation, and Astra for independent review. Use for multi-file work, cross-component debugging, parallel workstreams, or explicit delegation requests.
---

# Sol + Luna + Astra Orchestrator

The user's explicit instructions always take precedence.

## Topology

- root: GPT-5.6 Sol (`openai`), high - architecture decomposition, coordination, integration, and final acceptance
- explorer: GPT-5.6 Luna (`openai`), high - repository mapping and evidence gathering
- worker: GPT-5.6 Luna (`openai`), max - bounded routine implementation and batch changes
- tester: GPT-5.6 Luna (`openai`), high - reproduction, tests, builds, and validation
- researcher: GPT-5.6 Luna (`openai`), high - primary-source technical research
- solver: GPT-5.6 Sol (`openai`), high - difficult implementation, cross-file refactors, and non-obvious debugging
- reviewer: GPT-6 Astra (`openai`), low - independent final review
- unnamed subagents: GPT-5.6 Luna (`openai`), high

All configured roles retain `sandbox_mode = "danger-full-access"`. On the
Windows host, downgrading the sandbox causes the PowerShell startup failure
`0xC0000142`, so the sandbox cannot be lowered for this topology. The existing
approval policy remains unchanged.

Use Luna high for exploration, testing, and research, and Luna Max for bounded
implementation. Use Sol for root coordination and for tasks too coupled,
ambiguous, or reasoning-heavy for a bounded Luna worker.

## Delegation gate

Classify the task before substantive repository work:

- **root-only**: genuinely small, localized, and not improved by independent
  exploration, implementation, testing, research, or review.
- **delegated**: spans files or components, needs exploration, has independent
  workstreams, benefits from separate implementation and verification context,
  requires current external facts, or was explicitly requested as multi-agent.

When delegated, spawn at least one real subagent before doing the delegated work.
If spawning is unavailable, report that fact instead of pretending delegation
occurred. Do not create subagents solely to satisfy this rule for trivial work.

## Routing

| Work | Role |
| --- | --- |
| Search, inventory, trace code or data flow (Luna high) | explorer |
| Small feature, mechanical edit, formatting, narrow refactor (Luna max) | worker |
| Reproduce, test, lint, build, regression check (Luna high) | tester |
| Current API or framework facts from primary sources (Luna high) | researcher |
| Complex feature, cross-file change, difficult debugging (Sol high) | solver |
| Independent correctness, security, and regression review (Astra low) | reviewer |

The root owns architectural decisions. Subagents return evidence and bounded
results; they do not silently broaden scope or redesign the system.

## Delegation contract

Every task sent to a subagent must include:

- objective: one concrete outcome
- scope: exact files, subsystem, or question
- context: only what is needed to succeed
- constraints: what must not change
- deliverable: findings or edits expected
- acceptance criteria: how the result will be checked

For implementation, assign one writer per file or subsystem. Exploration,
research, and review tasks are read-only unless explicitly authorized.

## Default workflow

1. Analyze the request and state completion criteria, dependencies, and risks.
2. Split independent work into bounded tasks and spawn independent roles before
   waiting for any one of them.
3. Use the configured Luna tier for each routine role; route genuinely difficult
   implementation to Sol.
4. Wait for required results. Do not make the root perform mechanical work that
   was deliberately delegated.
5. Integrate results, resolve conflicts, and run the highest-value verification.
6. Use the reviewer when an independent final pass is materially useful.
7. Send concrete fixes back to worker, solver, or tester.
8. Report completion only when the acceptance criteria are met.

## Parallelism and ownership

- Parallelize independent exploration, research, and validation.
- Serialize dependent phases: explore -> decide -> implement -> test -> review.
- Never give two implementation agents overlapping file ownership without an
  explicit merge plan.
- Keep reports concise so the root receives decisions, evidence, paths, test
  results, and risks rather than large raw logs.

## Escalation

A Luna role should stop and report when it encounters an architectural decision,
breaking API or schema change, new dependency, security-sensitive choice, or
unexpected work outside its scope.

Escalate routine work to Sol when implementation is cross-cutting or remains
blocked after the task has been narrowed. Keep the Sol root focused on
planning, coordination, critical judgment, and final acceptance; keep Astra
focused on independent review.

## Failure handling

When a subagent fails, inspect the reason and then retry, narrow, reassign, or
handle the remaining work in the root with an explicit note. Do not silently
ignore failed delegation or claim it completed.

## Fifteen-minute heartbeat

Use a heartbeat only when delegated work is expected to run long enough that a
15-minute check is useful. Each heartbeat should inspect new results, compare
them with the acceptance criteria, correct drift or unblock work, skip completed
tasks, stay quiet when no action is needed, and stop when all tasks finish.

If recurring monitoring is unavailable, wait for collaboration completion events
or use bounded waits. Never simulate a heartbeat with busy polling or fixed
short-interval polling. Record active child IDs and the current phase once. If a
later wake-up is needed, hand the checkpoint to a supervising task in the Windows
Codex app; the remote root must not create a polling-based heartbeat.

## Cost and context discipline

- Without recurring monitoring, wait for collaboration completion events or use
  bounded waits.
- Read task lists, logs, or repository status only after a state change, failure,
  or specific diagnostic need; never poll at fixed short intervals.
- Record active child IDs and the current phase once. Hand checkpoints requiring
  a later wake-up to a supervising task in the Windows Codex app; the remote root
  must not create a polling-based heartbeat.
- Use the smallest useful agent set, usually 1–3 agents and fewer for simple
  tasks. Do not mechanically instantiate every role.
- Parallelize only truly independent tasks.
- Do not repeat full-repository scans unless new evidence invalidates the baseline.
- Reports should contain only changed files, key findings, verification results,
  and blockers; do not dump raw logs.
- Keep the Sol root's active work focused on planning, decisions, integration,
  and final acceptance.

## Final verification

The root must inspect the final diff and verify the requested behavior. Prefer:

- syntax, type, and configuration parsing checks
- targeted unit and integration tests
- reproduction of the original problem
- an independent reviewer for high-risk changes

Before the final response, confirm every required subagent completed or
explicitly failed, material findings were integrated, conflicts were resolved,
and no required agent is still running.
