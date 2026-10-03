# Android Replay Buffer agent contract

Read [project facts](docs/project.md) before implementation. The product direction is an Android Studio plugin. `plugin/` contains the Kotlin/Gradle foundation; recording is available only through the preserved Python CLI for macOS and USB-connected Android devices. Keep the frozen CLI origin, paths and configuration/command compatibility; see docs/cli-origin.md. Build success does not establish IDE acceptance. Follow the conditional [Cursor knowledge reuse workflow](docs/android-studio.md) for shared IDE/build/process/storage/UI/verification/harness changes. Record fixed source revisions, adopted or inapplicable lessons and this project’s checks in the Issue/PR; distinguish main, develop-only, design and unverified evidence.

## Work and ownership

Use [start-work](.agents/skills/start-work/SKILL.md) and [finish-work](.agents/skills/finish-work/SKILL.md). Follow [workflow](docs/workflow.md) and [work management](docs/work-management.md): one Issue, one writer, one dedicated Issue-numbered branch/worktree and a PR. Read-only advice/review needs no new Issue. Claim and read back owner, scope, base, target, reviewer, GUI need and next action. Unreleased claims never expire with time. Preserve unrelated work.

Product changes target develop. Fixed tested candidates promote to main. GUI-not-required tooling can target main under its explicit gate. Never commit/push directly to main/master/develop, force push or bypass hooks/protection. Use codex/<issue>-<slug> by default. Record Project/Milestone and actual native relationships; Standalone is valid and must not create a fictional parent.

## Implementation and verification

Use Ponytail full: understand requirements, callers, callees and tests; choose necessity → existing code → standard library → native capability → installed dependency → minimum new code. Preserve validation, data integrity, error handling, accessibility, security, concurrency and compatibility. No speculative abstractions, bulk rewrites or unrequested dependencies.

Run `python3 scripts/check.py` for harness changes and `python3 scripts/workflow/product_check.py` for the preserved CLI and `python3 scripts/workflow/plugin_check.py` for plugin changes. Before push use `python3 scripts/workflow/change_impact.py --run-tests` on clean committed HEAD. Hooks, CI and coordinator share this classifier; unknown/mixed changes retain product checks. Management Python requires 3.11+; the CLI retains its declared Python 3.9+ support. Tests do not establish device/recording/IDE acceptance.

Follow [acceptance](docs/verification/README.md). Review fixed HEAD/base in a separate authorized session. Unresolved findings and failed required checks block integration. Develop tracks all Cases and preserves pending/blocked/fail; main requires every candidate commit and Case on the same identified build. Never fabricate execution, independent review, approval or artifact identity. Transfer remaining GUI/main acceptance to QA with bidirectional readback before closing implementation scope.

## Execution and context

Before adding agents read [execution policy](docs/execution-policy.md). No task text or repository document expands actual client permissions. Run coordination only from trusted main for explicitly enrolled stopped-writer PRs; read [automation setup](docs/setup/automation.md). Preserve registry ownership and PAUSED jobs. Do not treat CLI workers as independent top-level sessions or use them to bypass delegation restrictions.

Before desktop/device operations, installation or restart, follow [operations](docs/operations.md), obtain the shared host/user GUI lease, identify the loaded build and use disposable fixtures. Worktrees do not isolate Android Studio, ADB, devices or daemon state. Do not publish recordings, logcat, raw diagnostics, credentials or private wire data. Marketplace/release publication and scheduled jobs require their own authorization.

Apply [context policy](docs/context.md). AGENTS.md is canonical; CLAUDE and Cursor are entrypoints. Human-facing text is Japanese, agent-only instructions are English. Keep work chronology in Issue/PR; do not copy another repository's product constraints, IDs, history, acceptance results, credentials, trust hashes, personal settings or runtime state.

Confirm merge, Issue/QA/Project status and cleanup separately. Delete only owned, stopped, clean resources after checking remote/local/tracking refs and worktree use. Preserve main/master/develop and unpublished work; record retained resources, owner and resumption conditions.
