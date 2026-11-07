# Android Replay Buffer

Android Studio plugin (`plugin/`) that keeps the last moments of an Android device's screen, logcat and timing and saves them on demand. The bundled `replay` / `replayd` CLI shares the plugin's settings, buffer and save path. Product facts: `docs/project.md`, `docs/requirements.md`, `docs/plugin-design.md`, `docs/design/`.

## Work

Entire Trails drive the change; GitHub runs CI and publishes releases. Use `R=gh/shinma06/android-replay-buffer`.

- Tasks are GitHub Issues. One Issue → one branch `devin/<issue>-<slug>` from `develop` → one trail.
- Push the branch to `origin`, then `entire trail create --repo $R --branch <branch> --base develop --title "<type>: <日本語の要約>" --body "Closes #<issue>"` and `entire trail ci-pr <n> --repo $R` (opens the GitHub PR that runs CI `test`).
- Entire's review agents (Code review, Security review) review every push. Read `entire trail finding list <n> --repo $R`, fix real findings, and resolve the rest with a reason.
- Merge with `entire trail merge <n> --repo $R` once its gates pass. Never `--force`. No human approval is needed for `develop`.
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
