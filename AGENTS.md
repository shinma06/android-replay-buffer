# Android Replay Buffer (ARB)

Records the last moments of an Android device's screen, logcat, taps and timing, and saves them on demand. Rust core daemon (`arbd`) + CLI/MCP (`arb-cli`) + tray (`arb-tray`), an Android `device-agent`, and a Compose/Jewel Android Studio plugin (`ide-plugin`) share one daemon and one save path. Product facts: `docs/v2/` (REQ / SPEC / VER, checked by `sara`).

## Layout

- `core/` — Rust workspace (`arbd`, `arb-cli`, `arb-tray`, `arb-core`, `arb-os`, `arb-adb`, `arb-media`, `arb-storage`) and `core/scripts/` (verify, license check, packaging, player matrix).
- `device-agent/` — on-device companion (`build.sh`, host protocol tests).
- `ide-plugin/` — Kotlin Gradle build: `client-core` (pure client library) + the IDE plugin (Compose/Jewel).
- `docs/v2/` — requirements, specs, verification records, testing plans. `docs/v2/sara.toml` is the traceability root.
- `fixtures/` — read-only legacy schema 3/4 bundle fixtures. `third-party/scrcpy/` — pinned Apache-2.0 scrcpy-server and the audio patch.
- `.github/workflows/` — `ci.yml` (checks + `early` prerelease), `release.yml` (stable tag release).

## Build and test

```bash
./core/scripts/verify.sh                                   # fmt, clippy -D warnings, test, license (zero GPL/AGPL), sara --strict, REQ coverage
./device-agent/build.sh --with-scrcpy-audio                # needs ANDROID_HOME with android-37.2 + build-tools
cargo build --manifest-path core/Cargo.toml --release -p arbd -p arb-cli -p arb-tray
(cd ide-plugin && ./gradlew test buildPlugin)              # needs JDK 27; bundles core/target/release + device-agent/build when present
```

Passing tests does not mean IDE/device acceptance; say which was verified and which was not (USB device, macOS/Windows IDE, emulator).

## Work

Entire Trails drive the change; GitHub runs CI and publishes releases. Always pass `--repo gh/shinma06/android-replay-buffer` to `entire trail` commands when `origin` goes through a proxy.

- Tasks are trails. One task → one branch → one trail, created with `entire trail create --branch <branch> --base <base> --title "<type>: <日本語の要約>" --body "..."`. Active feature integration targets `v2-develop`; `develop` is the integration line, `main` is stable.
- `entire trail ci-pr <n>` opens a draft PR; retitle it to the trail title, keep the `<!-- entire-shadow-pr -->` marker, write the summary, then `entire trail ready <n>`.
- Review with `codex review --base <base>`; post real issues with `entire trail finding add` (when none, add one `--severity low` finding saying so and resolve it). Fix real findings and resolve the rest with a reason, in both `entire trail finding list <n>` and the GitHub review threads.
- Merge with `entire trail merge <n>` once gates pass (findings, required checks, up to date). Never `--force`. Merge on Entire, not on GitHub. Afterwards close the trail with `entire trail update <n> --status closed`.
- Required CI check names are fixed by repository rulesets: `test`, `os-parity (macos-latest)`, `os-parity (windows-latest)`. Do not rename those jobs.
- `early` follows `develop` (CI `early` job); never edit it by hand. Stable releases are tags `vX.Y.Z` on `main` (SemVer) after VM IDE/emulator acceptance.
- Support floor: Android 9 (API 28)+ core guaranteed (API 28–29 degrade audio cleanly to `audio=false` / `Unavailable`), Android 11 (API 30)+ recommended for audio capture/playback, API ≤ 27 unsupported (non-blocking warning). IDE plugin targets Android Studio Rabbit (2026.2 / IntelliJ `262.9437.185`–`262.*`); older Android Studio and VS Code/Cursor use native `arb-tray` + `arb-cli` + MCP.
- Proceed on reversible decisions and report them; stop only for public exposure, data deletion, billing, credentials, or anything a platform gate requires.

## Rules

- Zero GPL/AGPL dependencies (`core/scripts/check-licenses.py`). Third-party code must be permissively licensed and recorded in the relevant `NOTICE`.
- Behaviour is identical on Linux, macOS and Windows. OS-specific code lives only in the per-OS implementations selected at a single OS-detection point (`arb-os`, `HostOs` in `client-core`); callers never branch on the OS. Tests must not skip by OS unless they exercise a truly OS-only facility.
- GUI, CLI and MCP behaviour stays identical; a change to one surface updates the others and the relevant `docs/v2/` spec.
- Legacy schema 3/4 bundles are read-only (`arb-storage`); never write or migrate them in place.
- Do not publish recordings, logcat, raw diagnostics or credentials.
- User-facing text (PRs, UI, docs) is Japanese; agent-only instructions are English.
- Entire records agent sessions. In every new worktree run `entire-agent-devin bootstrap --checkpoint-repo shinma06/entire-checkpoints` (Devin) so checkpoints go only to the private `checkpoints` remote, never `origin`. After each push upload them: `entire checkpoint list --outgoing --json | python3 -c "import sys,json;print(' '.join(c['checkpoint_id']+'@'+c['commit'] for c in json.load(sys.stdin).get('checkpoints',[])))" | xargs -r entire checkpoint push`. To pick up earlier work, run `entire checkpoint explain <id>` with the `Entire-Checkpoint` trailer of its commits.
