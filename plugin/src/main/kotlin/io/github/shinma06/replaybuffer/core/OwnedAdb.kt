package io.github.shinma06.replaybuffer.core

import java.io.ByteArrayOutputStream
import java.nio.file.Path
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/** Only directly launched processes are owned. Never restarts/kills the shared adb server. */
internal class OwnedAdb(private val executable: Path) : AutoCloseable {
    private val closing = AtomicBoolean()
    private val owned = ConcurrentHashMap.newKeySet<Process>()
    private val drains = ConcurrentHashMap<Process, Thread>()

    @Synchronized
    fun start(vararg args: String): Process {
        check(!closing.get())
        val process = ProcessBuilder(listOf(executable.toString()) + args).start()
        owned.add(process)
        drains[process] = thread(name = "replay-stderr", isDaemon = true) {
            runCatching { process.errorStream.use { stream ->
                val bytes = ByteArray(4096)
                while (stream.read(bytes) >= 0) { /* Drain without publishing raw device output. */ }
            } }
        }
        return process
    }

    fun command(vararg args: String, limit: Int = 1024 * 1024): String {
        val process = start(*args)
        val output = ByteArrayOutputStream()
        val failed = AtomicBoolean()
        val reader = thread(name = "replay-command-output", isDaemon = true) {
            runCatching { process.inputStream.use { stream ->
                val bytes = ByteArray(4096)
                while (true) {
                    val count = stream.read(bytes)
                    if (count < 0) break
                    check(output.size() + count <= limit)
                    output.write(bytes, 0, count)
                }
            } }.onFailure { failed.set(true); process.destroy() }
        }
        try {
            check(process.waitFor(5, TimeUnit.SECONDS)) { "adbの応答がタイムアウトしました" }
            reader.join(1000)
            check(!reader.isAlive && !failed.get() && process.exitValue() == 0) { "adbの要求に失敗しました" }
            return output.toString(Charsets.UTF_8)
        } finally { stop(process) }
    }

    @Synchronized
    fun stop(process: Process) {
        if (process !in owned) return
        val interrupted = Thread.interrupted()
        try {
            // Keep ownership until exit is confirmed, including on cancellation races.
            process.destroy()
            if (!process.waitFor(300, TimeUnit.MILLISECONDS)) {
                process.destroyForcibly()
                check(process.waitFor(2, TimeUnit.SECONDS)) { "所有adb clientの終了を確認できません" }
            }
            release(process, System.nanoTime() + 500_000_000)
        } finally { if (interrupted) Thread.currentThread().interrupt() }
    }

    private fun release(process: Process, deadline: Long) {
        check(!process.isAlive) { "所有adb clientの終了を確認できません" }
        runCatching { process.inputStream.close() }
        runCatching { process.outputStream.close() }
        runCatching { process.errorStream.close() }
        drains[process]?.let {
            TimeUnit.NANOSECONDS.timedJoin(it, (deadline - System.nanoTime()).coerceAtLeast(0))
            check(!it.isAlive) { "所有adb stderr readerの終了を確認できません" }
        }
        drains.remove(process)
        owned.remove(process)
    }

    @Synchronized
    override fun close() {
        closing.set(true)
        val interrupted = Thread.interrupted()
        var failure: Throwable? = null
        try {
            val processes = owned.toList()
            processes.forEach { runCatching { it.destroy() }.onFailure { e -> failure = e } }
            val graceful = System.nanoTime() + 300_000_000
            processes.forEach { runCatching {
                it.waitFor((graceful - System.nanoTime()).coerceAtLeast(0), TimeUnit.NANOSECONDS)
            }.onFailure { e -> failure = e } }
            processes.filter { it.isAlive }.forEach {
                runCatching { it.destroyForcibly() }.onFailure { e -> failure = e }
            }
            val forced = System.nanoTime() + 2_000_000_000
            processes.forEach { runCatching {
                check(it.waitFor((forced - System.nanoTime()).coerceAtLeast(0), TimeUnit.NANOSECONDS)) {
                    "所有adb clientの終了を確認できません"
                }
            }.onFailure { e -> failure = e } }
            val drained = System.nanoTime() + 500_000_000
            processes.forEach { runCatching { release(it, drained) }.onFailure { e -> failure = e } }
            failure?.let { throw it }
        } finally { if (interrupted) Thread.currentThread().interrupt() }
    }
}

internal fun serialArgument(serial: String): String {
    require(serial.length in 1..256 && Regex("[A-Za-z0-9._:-]+").matches(serial)) { "端末IDが不正です" }
    return serial
}

internal fun parseDevices(output: String): List<ReplayDevice> = output.lineSequence().drop(1).mapNotNull { line ->
    val fields = line.trim().split(Regex("\\s+"))
    if (fields.size < 2 || fields[1] != "device") return@mapNotNull null
    val serial = serialArgument(fields[0])
    val model = fields.firstOrNull { it.startsWith("model:") }?.substringAfter(':')?.replace('_', ' ') ?: serial
    ReplayDevice(serial, model.take(256), if (serial.startsWith("emulator-")) DeviceKind.EMULATOR else DeviceKind.PHYSICAL, true)
}.toList()
