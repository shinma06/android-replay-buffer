package io.github.shinma06.replaybuffer.core

import java.io.DataInputStream
import java.io.InputStream
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.nio.file.Path
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread
import kotlin.random.Random

internal class DeviceCapture(
    private val adbPath: Path,
    serial: String,
    private val resources: CaptureResources,
    private val store: CaptureStore,
    private val generation: Long,
    application: ApplicationTarget,
) : AutoCloseable {
    private val serial = serialArgument(serial)
    private val adb = OwnedAdb(adbPath)
    private val stopping = AtomicBoolean()
    private val timer = Executors.newSingleThreadScheduledExecutor { r -> Thread(r, "replay-watchdog").apply { isDaemon = true } }
    private val workers = mutableListOf<Thread>()
    private val sockets = ConcurrentHashMap.newKeySet<Socket>()
    private val ports = ConcurrentHashMap.newKeySet<String>()
    private val token = UUID.randomUUID().toString().replace("-", "")
    private val serverFile = "/data/local/tmp/replay-$token-server.jar"
    private val clockFile = "/data/local/tmp/replay-$token-clock.jar"
    private val serverName = "replay-$token-video"
    private val clockName = "replay-$token-clock"
    var cleanupPending: Boolean = false
        private set
    @Volatile private var application = application

    fun start() {
        launch("clock", ::clock)
        launch("video", ::video)
        launch("device_log", ::logs)
        workers += thread(name = "replay-app", isDaemon = true) {
            while (!stopping.get()) {
                val selected = application
                if (selected.packageName == null) {
                    store.app(null, null, emptySet(), generation)
                } else {
                    runCatching {
                        val packages = adb.command("-s", serial, "shell", "cmd", "package", "list", "packages", "-U", selected.packageName)
                        val uid = packages.lineSequence().mapNotNull {
                            val m = Regex("package:([A-Za-z0-9_.]+)\\s+uid:(\\d+)").matchEntire(it.trim())
                            if (m?.groupValues?.get(1) == selected.packageName) m.groupValues[2].toLong() else null
                        }.singleOrNull()
                        val ps = adb.command("-s", serial, "shell", "ps", "-A", "-o", "PID,UID,NAME")
                        val pids = ps.lineSequence().drop(1).mapNotNull {
                            val f = it.trim().split(Regex("\\s+"))
                            if (f.size >= 3 && (f[2] == selected.packageName || f[2].startsWith(selected.packageName + ":"))) f[0].toIntOrNull() else null
                        }.toSet()
                        if (selected == application) store.app(selected.packageName, uid, pids, generation)
                    }.onFailure { if (selected == application) store.app(selected.packageName, null, emptySet(), generation) }
                }
                if (!pause(1000)) break
            }
        }
    }

    fun application(value: ApplicationTarget) {
        value.validate()
        application = value
        store.app(value.packageName, null, emptySet(), generation)
    }

    private fun launch(kind: String, operation: () -> Unit) {
        workers += thread(name = "replay-$kind", isDaemon = true) {
            var delay = 0L
            val recovery = RecoveryDelay()
            while (!stopping.get()) {
                if (delay > 0 && !pause(delay)) break
                if (kind != "clock") store.status(kind, StreamState.RECOVERING, "取得を復旧しています", generation)
                val attempt = System.nanoTime()
                runCatching(operation).onFailure {
                    if (!stopping.get() && kind != "clock") store.status(kind, StreamState.RECOVERING,
                        if (kind == "video") "動画取得が中断しました（PTS/config/容量/接続を確認してください）" else "全体ログ取得が中断しました", generation)
                }
                delay = recovery.next(System.nanoTime() - attempt >= 10_000_000_000)
            }
        }
    }

    private fun video() {
        adb.command("-s", serial, "push", resources.server.toString(), serverFile)
        val scid = Random.nextInt(1, Int.MAX_VALUE).toString(16).padStart(8, '0')
        val port = adb.command("-s", serial, "forward", "tcp:0", "localabstract:scrcpy_$scid").trim()
        require(Regex("[0-9]{1,5}").matches(port) && port.toInt() in 1..65535)
        ports.add(port)
        // All shell tokens are fixed or generated hex; no user-supplied shell text is interpolated.
        val server = adb.start("-s", serial, "shell", "CLASSPATH=$serverFile app_process / --nice-name=$serverName com.genymobile.scrcpy.Server 4.0 " +
            "scid=$scid tunnel_forward=true audio=false control=false video_codec=h264 send_device_meta=false " +
            "send_frame_meta=true send_stream_meta=true max_size=1920 max_fps=30 video_bit_rate=8000000 " +
            "video_codec_options=max-bframes:int=0,i-frame-interval:int=1")
        val socket = Socket()
        sockets.add(socket)
        try {
            socket.soTimeout = 3000
            socket.connect(InetSocketAddress(InetAddress.getLoopbackAddress(), port.toInt()), 3000)
            val input = DataInputStream(socket.getInputStream())
            require(input.readUnsignedByte() == 0)
            require(input.readInt() == 0x68323634) { "固定H.264以外のcodecです" }
            while (!stopping.get()) {
                when (val packet = readVideo(input)) {
                    is VideoPacket.Session -> store.session(packet, generation)
                    is VideoPacket.Frame -> store.frame(packet, generation)
                }
            }
        } finally {
            socket.close()
            sockets.remove(socket)
            adb.stop(server)
            runCatching { adb.command("-s", serial, "forward", "--remove", "tcp:$port") }.onSuccess { ports.remove(port) }
        }
    }

    private fun logs() {
        val deadline = System.nanoTime() + 5_000_000_000
        while (store.clock.snapshot().none { it.valid } && System.nanoTime() < deadline) { check(pause(50)) }
        val started = System.nanoTime()
        val initial = store.clock.snapshot().lastOrNull { it.valid }
        val lower = initial?.let { it.wall + (started - it.received).coerceAtLeast(0) }
        val process = adb.start("-s", serial, "logcat", "-b", "all", "-B", "-T", "1")
        try {
            store.status("device_log", StreamState.CAPTURING, null, generation)
            var live = false
            while (!stopping.get()) {
                val record = readLog(process.inputStream)
                // Exclude the old -T 1 tail; clock jumps remain explicitly uncertain, never backfilled.
                val anchor = store.clock.snapshot().lastOrNull { it.valid }
                if (lower == null || anchor?.epoch != initial.epoch || record.wall >= lower) live = true
                if (live) store.log(record, generation)
            }
        } finally { adb.stop(process) }
    }

    private fun clock() {
        val boot = adb.command("-s", serial, "shell", "settings", "get", "global", "boot_count").trim()
        require(Regex("[0-9]{1,12}").matches(boot)) { "boot時計を識別できません（API24以上が必要です）" }
        adb.command("-s", serial, "push", resources.clock.toString(), clockFile)
        val process = adb.start("-s", serial, "shell", "CLASSPATH=$clockFile app_process / --nice-name=$clockName io.github.shinma06.replaybuffer.clock.ClockProbe")
        try {
            val startup = timer.schedule({ runCatching { adb.stop(process) } }, 3, TimeUnit.SECONDS)
            try { require(readClockLine(process.inputStream) == "REPLAY_CLOCK_1") } finally { startup.cancel(false) }
            var count = 0
            while (!stopping.get()) {
                val nonce = UUID.randomUUID().toString().replace("-", "")
                val sent = System.nanoTime()
                val expiry = timer.schedule({ runCatching { adb.stop(process) } }, 3, TimeUnit.SECONDS)
                try {
                    process.outputStream.write((nonce + "\n").toByteArray(Charsets.US_ASCII))
                    process.outputStream.flush()
                    val fields = readClockLine(process.inputStream).split('\t')
                    require(fields.size == 5 && fields[0] == nonce)
                    store.clock.add(boot, fields.drop(1).map { it.toLong() }, sent, System.nanoTime())
                } finally { expiry.cancel(false) }
                count++
                if (count >= 5 && !pause(1000)) break
            }
        } finally { adb.stop(process) }
    }

    private fun pause(millis: Long): Boolean = try { Thread.sleep(millis); !stopping.get() } catch (_: InterruptedException) { false }

    override fun close() {
        stopping.set(true)
        sockets.forEach { runCatching { it.close() } }
        timer.shutdownNow()
        adb.close()
        workers.forEach { it.interrupt() }
        workers.forEach { it.join(2500) }
        check(workers.none { it.isAlive }) { "取得readerの終了を確認できません" }
        cleanupPending = !cleanupRemote()
    }

    // Names include this generation's UUID; a PID is used only after matching that exact owned name.
    fun cleanupRemote(): Boolean = runCatching {
        OwnedAdb(adbPath).use { cleanup ->
            fun ownedPids(): Set<Int> = cleanup.command("-s", serial, "shell", "ps", "-A", "-o", "PID,ARGS")
                .lineSequence().drop(1).mapNotNull { line ->
                    val fields = line.trim().split(Regex("\\s+"))
                    if (fields.size >= 2 && fields[1] in setOf(serverName, clockName)) fields[0].toIntOrNull()?.takeIf { it > 1 } else null
                }.toSet()
            ownedPids().forEach { pid ->
                // Re-check identity immediately before signalling, never guess a shared scrcpy/app_process PID.
                if (pid in ownedPids()) cleanup.command("-s", serial, "shell", "kill", "-TERM", pid.toString())
            }
            check(ownedPids().isEmpty()) { "所有端末processの終了が未確認です" }
            ports.toList().forEach { cleanup.command("-s", serial, "forward", "--remove", "tcp:$it"); ports.remove(it) }
            cleanup.command("-s", serial, "shell", "rm", "-f", serverFile, clockFile)
        }
        cleanupPending = false
        true
    }.getOrDefault(false)

    fun cleanupMarker(): Map<String, Any> = mapOf("serial" to serial, "generation" to generation,
        "server_name" to serverName, "clock_name" to clockName, "remote_files" to listOf(serverFile, clockFile), "forwards" to ports.toList())

}

internal fun readClockLine(input: InputStream): String {
    val bytes = java.io.ByteArrayOutputStream()
    while (true) {
        val b = input.read()
        check(b >= 0) { "時計helperの出力が途切れました" }
        if (b == 10) return bytes.toString(Charsets.US_ASCII).trimEnd('\r')
        check(bytes.size() < 512)
        bytes.write(b)
    }
}

internal class RecoveryDelay {
    private var failures = 0
    fun next(healthy: Boolean): Long {
        if (healthy) failures = 0
        return when (failures++) { 0 -> 0L; 1 -> 1000L; 2 -> 2000L; 3 -> 4000L; else -> 5000L }
    }
}
