# Third-Party Notices & Licensing Attribution (`device-agent`)

`device-agent` is strictly GPL/AGPL-free and combines:

1. **ARB Device Companion (`device-agent/companion/`)**
   - Original source code owned by Android Replay Buffer (no external runtime dependencies; compiled against Android SDK API stubs via `d8` targeting min API 24).

2. **scrcpy server (`scrcpy-server-v5.0.1`) — Apache License 2.0**
   - Upstream project: Genymobile / Romain Vimont — [scrcpy](https://github.com/Genymobile/scrcpy)
   - Source revision: `a60891aea193d92e7e5c3942700eca63f9d19a5f` (`v5.0.1`)
   - Official release: <https://github.com/Genymobile/scrcpy/releases/tag/v5.0.1>
   - Unmodified binary in repository: `plugin/src/main/resources/replay/deps/scrcpy-server-v5.0.1`
   - Expected SHA-256: `764eb6f79811d5211fe9df341120882ba9994c7a61b897d7bf3fb662e53bc536`
   - License text in repository: `plugin/src/main/resources/replay/licenses/scrcpy-LICENSE.txt` (Apache License 2.0)

3. **Patched `AudioRecordReader` — Apache License 2.0**
   - Source in repository: `plugin/src/scrcpy/java/com/genymobile/scrcpy/audio/AudioRecordReader.java`
   - Derived from `scrcpy` revision `a60891aea193d92e7e5c3942700eca63f9d19a5f` under Apache License 2.0.
   - Modification summary: computes each audio buffer's PTS from `AudioRecord.getTimestamp(timestamp, AudioTimestamp.TIMEBASE_MONOTONIC)` `(framePosition, nanoTime)` to represent the buffer's first frame rather than the latest captured frame.
   - Packaging recipe (`device-agent/build.sh --with-scrcpy-audio`): compiles `AudioRecordReader.java` with `d8 --release --min-api 24` into `classes.dex` and merges it with the verified, unmodified `scrcpy-server-v5.0.1` entries (with the upstream `classes.dex` renamed to `classes2.dex`) to produce `device-agent/build/replay-scrcpy-audio.dex.jar`. Because `app_process` loads `classes.dex` before `classes2.dex` and scrcpy's `CleanUp` re-invokes only the first `CLASSPATH` entry, a single merged DEX JAR replaces only `AudioRecordReader` cleanly while leaving the upstream server bytecode intact.

4. **Build-Only Tools (Not Shipped)**
   - Android SDK platform stubs (`android.jar`, Apache License 2.0)
   - Android SDK Build-Tools `d8` (BSD 3-Clause / Apache License 2.0)
