package io.github.shinma06.arb.ide

import com.google.gson.GsonBuilder
import com.google.gson.JsonArray
import com.google.gson.JsonObject
import com.intellij.openapi.project.Project
import io.github.shinma06.arb.ipc.HostOs
import java.io.File
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths

/**
 * Pure, headless-testable helper resolving screenshot output destinations:
 * - When `inheritLogcat == true` (default): queries Android Studio's `DeviceScreenshotSettings`
 *   via [LogcatScreenshotSettings] and falls back cleanly to `null` (`arbd`'s `output_dir`) if
 *   unavailable or if Studio's ABI changes.
 * - When `inheritLogcat == false`: uses `customScreenshotDir` if absolute and non-blank, otherwise
 *   falls back to `null` (`output_dir`).
 */
object ArbScreenshotDestinationResolver {
    data class Resolution(
        val directory: Path?,
        val inheritedFromLogcat: Boolean,
        val statusText: String,
    )

    fun resolveDirectory(
        project: Project?,
        inheritLogcat: Boolean,
        customScreenshotDir: String,
        logcatLocationProvider: () -> String? = { LogcatScreenshotSettings.saveLocationOrNull() },
        logcatDirectoryResolver: (Project?, String) -> Path? = { proj, loc ->
            LogcatScreenshotSettings.resolveDirectoryOrNull(proj, loc)
        },
    ): Resolution {
        if (inheritLogcat) {
            val rawLoc = runCatching { logcatLocationProvider() }.getOrNull()
            val resolved = rawLoc?.let { loc -> runCatching { logcatDirectoryResolver(project, loc) }.getOrNull() }
            return if (resolved != null) {
                Resolution(
                    directory = resolved,
                    inheritedFromLogcat = true,
                    statusText = "Logcat 継承先: $resolved",
                )
            } else {
                Resolution(
                    directory = null,
                    inheritedFromLogcat = false,
                    statusText = "Logcat の保存先を取得できないため既定の保存先 (output_dir) へ保存します",
                )
            }
        }

        val trimmed = customScreenshotDir.trim()
        if (trimmed.isEmpty()) {
            return Resolution(
                directory = null,
                inheritedFromLogcat = false,
                statusText = "未指定時は既定の保存先 (output_dir) へ保存します",
            )
        }
        val path = runCatching { Path.of(trimmed) }.getOrNull()
        return if (path != null && path.isAbsolute) {
            Resolution(
                directory = path.normalize(),
                inheritedFromLogcat = false,
                statusText = "指定フォルダへ PNG を保存します: ${path.normalize()}",
            )
        } else {
            Resolution(
                directory = null,
                inheritedFromLogcat = false,
                statusText = "絶対パスを指定してください（未設定時は既定の保存先へフォールバック）",
            )
        }
    }

    /**
     * Resolves the `save.screenshot` `output_dir` from the IDE-local screenshot settings (Android
     * Studio Logcat screenshot destination inheritance, or the custom folder when inheritance is
     * unchecked). Returns `null` so `arbd` applies its own `screenshot_dir_override` ->
     * `output_dir_override` -> `<data_dir>/screenshots` chain, keeping file naming identical to the
     * CLI / MCP / tray. Call off the EDT when passing a live [project].
     */
    fun resolveOutputDir(
        project: Project?,
        localSettings: ArbLocalSettingsState? = ArbLocalSettingsState.getInstanceOrNull(),
        logcatLocationProvider: () -> String? = { LogcatScreenshotSettings.saveLocationOrNull() },
        logcatDirectoryResolver: (Project?, String) -> Path? = { proj, loc ->
            LogcatScreenshotSettings.resolveDirectoryOrNull(proj, loc)
        },
    ): String? {
        val res = resolveDirectory(
            project = project,
            inheritLogcat = localSettings?.screenshotInheritLogcat ?: true,
            customScreenshotDir = localSettings?.customScreenshotDir.orEmpty(),
            logcatLocationProvider = logcatLocationProvider,
            logcatDirectoryResolver = logcatDirectoryResolver,
        )
        return res.directory?.toAbsolutePath()?.normalize()?.toString()
    }
}

/**
 * Pure, headless-testable resolver for the bundled `arb-cli` binary path and MCP configuration JSON
 * used by Settings -> Tools -> Android Replay Buffer ("CLI / MCP パスをコピー").
 */
object ArbCliMcpPathResolver {
    private val prettyGson = GsonBuilder().disableHtmlEscaping().setPrettyPrinting().create()

    fun cliExecutableName(os: HostOs): String = os.executableName("arb-cli")

    /**
     * Resolves the bundled `<pluginDir>/native/<os>-<arch>/arb-cli[.exe]` path, falling back to
     * dev builds (`core/target/{release,debug}`) or `PATH`, or finally returning the expected
     * bundled path when [pluginDir] is known so users always see a concrete path.
     */
    fun resolveCliPath(
        pluginDir: Path?,
        os: HostOs = HostOs.detect(),
        arch: String = HostOs.detectArch(),
        searchRoot: Path? = null,
        env: (String) -> String? = System::getenv,
    ): Path? {
        val exeName = cliExecutableName(os)
        if (pluginDir != null) {
            val slug = HostOs.platformBinSlug(os, arch)
            val slugs = if (arch == "x64") listOf(slug, "${os.bundleSlug}-x86_64") else listOf(slug)
            val candidates = slugs.flatMap { s ->
                listOf(
                    pluginDir.resolve("native").resolve(s).resolve(exeName),
                    pluginDir.resolve("bin").resolve(s).resolve(exeName),
                )
            }
            for (c in candidates) {
                if (Files.isRegularFile(c)) {
                    return c.toAbsolutePath().normalize()
                }
            }
        }

        // Dev checkout fallback
        var cur: Path? = (searchRoot ?: Paths.get("")).toAbsolutePath().normalize()
        repeat(6) {
            val dir = cur ?: return@repeat
            val rel = dir.resolve("core").resolve("target").resolve("release").resolve(exeName)
            if (Files.isRegularFile(rel)) return rel.toAbsolutePath().normalize()
            val dbg = dir.resolve("core").resolve("target").resolve("debug").resolve(exeName)
            if (Files.isRegularFile(dbg)) return dbg.toAbsolutePath().normalize()
            cur = dir.parent
        }

        // PATH fallback
        val pathEnv = env("PATH").orEmpty()
        for (entry in pathEnv.split(File.pathSeparatorChar)) {
            if (entry.isBlank()) continue
            val candidate = runCatching { Paths.get(entry).resolve(exeName) }.getOrNull() ?: continue
            if (Files.isRegularFile(candidate)) {
                return candidate.toAbsolutePath().normalize()
            }
        }

        // Expected bundled path hint if pluginDir is present even before packaging native binaries
        if (pluginDir != null) {
            val slug = HostOs.platformBinSlug(os, arch)
            return pluginDir.resolve("native").resolve(slug).resolve(exeName).toAbsolutePath().normalize()
        }
        return null
    }

    /**
     * Builds the canonical MCP server configuration JSON snippet:
     * `{"command": "<path>/arb-cli", "args": ["mcp"]}` (properly JSON-escaped for all 3 OSes).
     */
    fun formatMcpConfigJson(cliPath: String): String {
        val obj = JsonObject().apply {
            addProperty("command", cliPath)
            add("args", JsonArray().apply { add("mcp") })
        }
        return prettyGson.toJson(obj)
    }
}
