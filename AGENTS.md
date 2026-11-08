# Android Replay Buffer

Android Studio plugin (`plugin/`) that keeps the last moments of an Android device's screen, logcat and timing and saves them on demand. The bundled `replay` / `replayd` CLI shares the plugin's settings, buffer and save path. Product facts: `docs/project.md`, `docs/requirements.md`, `docs/plugin-design.md`, `docs/design/`.

## Work

Entire Trails drive the change; GitHub runs CI and publishes releases. Always pass `--repo gh/shinma06/android-replay-buffer` to `entire trail` commands: in Devin, `origin` goes through a proxy URL that Entire cannot map to this repository.

- Tasks are trails, not GitHub Issues. One task → one branch `devin/<slug>` from `develop` → one trail. Existing Issues are background only: when a change achieves one's purpose or makes it unnecessary, comment why and close it.
- Push the branch to `origin`, then `entire trail create --branch <branch> --base develop --title "<type>: <日本語の要約>" --body "<目的と変更の要約>"`.
- `entire trail ci-pr <n>` opens a draft GitHub PR titled with the branch name. Edit its title to the trail title and write the change summary in its body (keep the `<!-- entire-shadow-pr -->` marker), then `entire trail ready <n>` so reviews and CI `test` run.
- Review: `codex review --base develop` (uses `OPENAI_API_KEY`) is primary; post each real issue to the trail with `entire trail finding add`. The findings gate waits until the current head has a review, so when there are no findings, add one `--severity low` finding saying so and resolve it. If Codex cannot run (quota, outage), have a separate Devin session review the diff and post its findings the same way. (`entire review` asks for human approval on every push because Devin's git `user.email` differs from the commit author.)
- Before merging, fix real findings and resolve the rest with a reason, both in `entire trail finding list <n>` and in GitHub PR review threads (the `develop` ruleset requires resolved threads).
- Merge with `entire trail merge <n>` once its gates (findings, `test`, up to date) pass. Never `--force`. No human approval is needed for `develop`. If it fails with `merge_not_mergeable` while every gate passes, squash-merge the PR on GitHub (the `develop` ruleset allows squash only); the trail completes on its own.
- `develop` → `main` is a release: open the PR and merge it only after the user confirms the candidate on a real device/IDE. Tag `vX.Y.Z` on `main` to publish a GitHub Release.
- Proceed on reversible decisions and report them; stop only for public exposure, data deletion, billing, credentials, or anything a platform gate requires.

## Build and test

Needs JDK 27 (`JAVA_HOME`) and Android platform `android-37.2` under `ANDROID_HOME` (both provided by the Devin blueprint).

```bash
python3 scripts/workflow/plugin_check.py              # tests + plugin ZIP + structure check (what CI runs)
python3 scripts/workflow/plugin_check.py --tests-only # faster loop
```

Passing tests does not mean IDE/device acceptance; say which was verified.

## Rules

- Keep the CLI's commands, configuration and exit codes compatible (`docs/cli.md`).
- Do not publish recordings, logcat, raw diagnostics or credentials.
- User-facing text (Issues, PRs, UI, docs) is Japanese; agent-only instructions are English.
- Entire records Devin sessions. In every new worktree run `entire-agent-devin bootstrap --checkpoint-repo shinma06/entire-checkpoints`; checkpoints must only go to the private `checkpoints` remote, never `origin`.

The previous multi-agent operation is preserved; see `docs/legacy-ops/README.md`.
