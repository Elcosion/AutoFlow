<!-- BEGIN CODEX PRO WORKFLOW -->
# Codex Pro workflow

For coding tasks, use the `astra-orchestrator` skill when its trigger conditions match.
User instructions and task-specific safety restrictions always take precedence.

## Stable root and role topology

Keep the root agent on GPT-5.6 Sol with high reasoning for the entire thread. The
root owns architecture, risk classification, decomposition, integration, and
final acceptance. Do not recommend switching the root model or reasoning effort
mid-thread: preserving its prompt-cache continuity is part of the workflow.

Named roles:

- `auditor`: GPT-5.6 Luna, max, read-only. Before implementation, independently
  challenge material user assumptions and proposed fixes, classify claims as
  `CONFIRMED`, `PARTIALLY_CONFIRMED`, `CONTRADICTED`, or `UNKNOWN`, and produce a
  concise behavior contract. A material semantic conflict blocks implementation
  until the root resolves it with the user or stronger evidence.
- `explorer`, `tester`, and `researcher`: GPT-5.6 Luna, high.
- `worker`: GPT-5.6 Luna, max, for bounded implementation and batch changes.
- `solver`: GPT-5.6 Sol, high, for coupled cross-file implementation and difficult debugging.
- `reviewer`: GPT-6 Astra, low, read-only, for economical design or diff review.
- `reviewer_high`: GPT-6 Astra, high, read-only, for independent high-risk final review.

## R0-R3 risk routing

Classify by semantic and operational risk, not merely diff size:

- **R0 — mechanical:** root-only. Use for localized, behavior-preserving edits
  with direct verification.
- **R1 — bounded:** auditor -> one bounded writer (`worker`, or root when truly
  smaller) -> tester. Stop before writing if the audit finds a material semantic conflict.
- **R2 — cross-file:** auditor -> explorer -> one writer (`worker` or `solver` as
  complexity requires) -> tester -> `reviewer` at Astra low -> root acceptance.
- **R3 — high-risk:** auditor -> explorer -> pre-implementation `reviewer` at
  Astra low for design review -> `solver` at Sol high as the single writer ->
  tester -> independent `reviewer_high` at Astra high -> root gate and any
  required manual acceptance.

Raise risk for security/permissions, destructive or irreversible operations,
public API/schema changes, data integrity, concurrency, deployment, or a large
blast radius. Safety restrictions are task-specific; do not turn constraints
from one task into blanket defaults for later work.

## Git and resumable execution

At task start, inspect `git status`, current branch, and `HEAD`. Work on a
`codex/<task>` branch. When commits are authorized, make reversible logical
checkpoint commits after inspecting the staged diff. Exclude secrets,
machine-local configuration, binaries, logs, build directories, and generated
acceptance artifacts. Never auto-push. Prefer a new revert commit over rewriting
or discarding shared history. The installer for this workflow must never create
branches or commits.

For long or resumable work, store a compact state record under the repository's
Git directory, for example the path returned by:

```text
git rev-parse --git-path codex-tasks/<task>/state.json
```

Record the risk level, phase, branch/HEAD, active child IDs, file ownership,
completed checks, and next action. This state is operational metadata and must
not be tracked.

## Context and coordination discipline

Use the smallest useful role set and one writer per file or subsystem. Require
concise structured child reports containing decision/evidence, files changed,
verification, and remaining risks. Do not repeat full-repository scans, full
test suites, or unchanged reviews; after a fix, re-review only the delta and any
affected boundary. Prefer completion events and bounded waits. For delegated
work expected to exceed 15 minutes, use a 15-minute heartbeat only when the
surface supports recurring monitoring; otherwise keep resumable state and do
not simulate a heartbeat with polling.

## Acceptance gates

Keep automated verification and manual acceptance distinct. Report exact
commands and results for automated tests, builds, linters, and diff checks.

For a desktop task where the user needs a runnable candidate, the final gate
must run that target project's documented build command and report the EXE's
absolute path, build time, byte size, and SHA-256. Keep the executable and other
build output out of Git. Label manual acceptance explicitly as `pending` until
the user exercises the candidate, or `passed` only after the user confirms it.
Do not substitute an automated result for manual acceptance.

Do not delegate trivial work merely for parallelism, and do not let multiple
implementation agents edit overlapping files without an explicit ownership and
merge plan.
<!-- END CODEX PRO WORKFLOW -->

## Remote host compatibility

On this Windows host, keep `sandbox_mode = "danger-full-access"` for the root
and every named role. Lower sandbox modes trigger PowerShell startup failure
`0xC0000142`. This is a host-compatibility requirement and does not broaden
task scope or override user instructions.
