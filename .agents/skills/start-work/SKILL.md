---
name: start-work
description: Begin or resume an Issue-scoped implementation, documentation or configuration change with ownership and acceptance tracking. Not for read-only advice or review.
---

# Start work

Read AGENTS.md, docs/project.md, docs/work-management.md and docs/workflow.md from the repository root. Reuse unchanged context. For changes covered by docs/android-studio.md, inspect the relevant current Cursor implementation/tests and linked Issue/PR evidence before design; record a fixed source revision and applicability in this Issue. Unrelated CLI-only or wording work does not require a full upstream survey. For instructions/Skills also read docs/context.md.

Inspect the Issue/comments, related PRs, ownership, dependencies, branch, worktrees and dirty state. Search before creating an Issue. Claim scope, owner, base, target, reviewer, GUI need and next action, then read back. Unreleased claims do not expire. Resume in your existing worktree; take over only after reassignment and old-writer stop, in a separate worktree.

Fetch without disturbing edits. Branch from the configured integration branch into `agent|codex|claude|cursor/<issue>-<slug>` in a dedicated worktree. Remove an inherited upstream to the integration branch. Install hooks with `python3 scripts/bootstrap.py` after inspecting them; preserve custom hooks on conflict.

Record observable acceptance, real project checks and remaining GUI/release conditions in the Issue. Create a Draft PR on the first meaningful push. Continue through finish-work; setup or a Draft PR alone is not completion. Referencing agents, server settings or GUI capabilities does not authorize their use.

For team work, use docs/team-communication.md and the supplied PM/liaison routes. Keep technical results in concise agent packets; route genuine human decisions through PM to the liaison using native choice cards. Do not turn routine work or handoffs into new human approval gates. Preserve higher-priority output and actual tool permission requirements.

Apply docs/verification/responsibility.md to existing and future tests. Identify our owned guarantee and observable defect before choosing the minimum faithful method; use trusted effective contracts, preserve immutable originals/failures and distinguish full-file proof from representative native integration. Do not create new approval gates or rebuild/retest unchanged artifacts merely for an explicitly reviewed method revision. Reassessment needs exact provenance, conditions and complete effective coverage, not relabeled old PASS.

Use guarantee → required evidence/precision → minimum feasible method for all test families. Read the trusted effective method revision; do not reinstate historical counts/deadlines/same-run/instant-cleanup recipes. Keep owned data/timing/content, failure/atomicity/pin, ownership and actual integration. Reuse unchanged artifact-bound facts and test only missing purposes; method revision is not execution. Review necessity and coverage, including separate native/regression/FILE facts, rather than conformity to an obsolete recipe.
