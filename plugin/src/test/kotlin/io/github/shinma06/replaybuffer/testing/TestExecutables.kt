package io.github.shinma06.replaybuffer.testing

import java.io.File
import java.nio.file.Files
import java.nio.file.Path

object TestExecutables {
    @JvmField val windows: Boolean = System.getProperty("os.name").startsWith("Windows")

    private val python: String by lazy {
        val names = if (windows) listOf("python.exe", "python3.exe") else listOf("python3", "python")
        System.getenv("PATH").orEmpty().split(File.pathSeparator).asSequence().flatMap { directory ->
            names.asSequence().map { Path.of(directory, it) }
        }.firstOrNull { Files.isRegularFile(it) && Files.isExecutable(it) }?.toString() ?: error("python is required by test fixtures")
    }

    /**
     * Writes a Python fixture at [path] and returns the program to run. Windows cannot run a shebang script,
     * so there it becomes `<name>.py` started by `<name>.cmd`; `__file__` stays in the same directory.
     */
    @JvmStatic
    fun python(path: Path, source: String): Path {
        if (!windows) {
            Files.writeString(path, "#!/usr/bin/env python3\n" + source.removeShebang())
            check(path.toFile().setExecutable(true))
            return path
        }
        val script = path.resolveSibling("${path.fileName}.py")
        Files.writeString(script, source.removeShebang())
        return Files.writeString(path.resolveSibling("${path.fileName}.cmd"), "@\"$python\" \"%~dp0${script.fileName}\" %*\r\n")
    }

    /** A live child process that does nothing for [seconds]. */
    @JvmStatic
    fun sleeping(seconds: Int): Process = ProcessBuilder(python, "-c", "import time; time.sleep($seconds)").start()

    private fun String.removeShebang() = if (startsWith("#!")) substringAfter('\n') else this
}
