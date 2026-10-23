package io.github.shinma06.replaybuffer.cli

import java.io.DataInputStream
import java.io.DataOutputStream
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.JarURLConnection
import java.net.ServerSocket
import java.net.Socket
import java.nio.channels.Channels
import java.nio.channels.FileChannel
import java.nio.file.FileAlreadyExistsException
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.StandardOpenOption.CREATE_NEW
import java.nio.file.StandardOpenOption.READ
import java.nio.file.StandardOpenOption.WRITE
import java.nio.file.attribute.PosixFileAttributes
import java.nio.file.attribute.PosixFilePermissions
import java.security.MessageDigest
import java.time.Instant
import java.util.HexFormat
import java.util.Properties
import java.util.UUID
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.SynchronousQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

internal data class CliEndpoint(
    val id: String,
    val project: String,
    val build: String,
    val port: Int,
    val token: ByteArray,
    val pid: Long,
    val started: Instant,
) {
    fun isLive(): Boolean = ProcessHandle.of(pid).map { it.isAlive && it.info().startInstant().orElse(null) == started }.orElse(false)
}

/**
 * Current-user-only endpoint descriptors, separate from legacy replayd and IDE lock/socket formats.
 * Selection rejects unsafe, ambiguous or mismatched descriptors without repairing or deleting them.
 */
internal object ReplayCliRegistry {
    private val directoryMode = PosixFilePermissions.fromString("rwx------")
    private val fileMode = PosixFilePermissions.fromString("rw-------")
    private const val MAX_ENDPOINT_BYTES = 32 * 1024

    fun directory(): Path = Path.of(System.getProperty("user.home"), ".android-replay-buffer-cli")

    /** Hashes the containing local distribution JAR, not a version label or source revision. */
    fun buildIdentity(): String {
        // IDE class loaders can omit CodeSource.location while retaining the class resource URL.
        val resource = ReplayCliRegistry::class.java.getResource("ReplayCliRegistry.class")
        require(resource?.protocol == "jar") { "配布物のCLIを利用してください。" }
        val connection = resource.openConnection()
        require(connection is JarURLConnection && connection.jarFileURL.protocol == "file") {
            "配布物のCLIを利用してください。"
        }
        val jar = Path.of(connection.jarFileURL.toURI())
        require(Files.isRegularFile(jar)) { "配布物のCLIを利用してください。" }
        val hash = MessageDigest.getInstance("SHA-256")
        Files.newInputStream(jar).use { input ->
            val buffer = ByteArray(8192)
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                hash.update(buffer, 0, count)
            }
        }
        return HexFormat.of().formatHex(hash.digest())
    }

    fun checkPrivate(path: Path, directory: Boolean): PosixFileAttributes {
        val attributes = Files.readAttributes(path, PosixFileAttributes::class.java, NOFOLLOW_LINKS)
        val owner = path.fileSystem.userPrincipalLookupService.lookupPrincipalByName(System.getProperty("user.name"))
        require(attributes.owner() == owner && attributes.permissions() == if (directory) directoryMode else fileMode) {
            "CLI接続情報は現在の利用者だけがアクセスできる必要があります。"
        }
        require(if (directory) attributes.isDirectory else attributes.isRegularFile) { "CLI接続情報の形式が不正です。" }
        return attributes
    }

    fun prepare(directory: Path) {
        try {
            Files.createDirectory(directory, PosixFilePermissions.asFileAttribute(directoryMode))
        } catch (_: FileAlreadyExistsException) {
            // Never repair another owner's directory or follow a substituted symbolic link.
        }
        checkPrivate(directory, true)
    }

    fun publish(directory: Path, endpoint: CliEndpoint): Path {
        checkPrivate(directory, true)
        val path = directory.resolve("${endpoint.id}.properties")
        val properties = Properties().apply {
            setProperty("protocol", ReplayCliProtocol.VERSION.toString())
            setProperty("id", endpoint.id)
            setProperty("project", endpoint.project)
            setProperty("build", endpoint.build)
            setProperty("port", endpoint.port.toString())
            setProperty("token", HexFormat.of().formatHex(endpoint.token))
            setProperty("pid", endpoint.pid.toString())
            setProperty("started", endpoint.started.toString())
        }
        val staging = directory.resolve(".${endpoint.id}.tmp")
        val channel = FileChannel.open(staging, setOf(CREATE_NEW, WRITE, NOFOLLOW_LINKS), PosixFilePermissions.asFileAttribute(fileMode))
        try {
            channel.use { properties.store(Channels.newOutputStream(it), "Android Replay Buffer CLI") }
            Files.createLink(path, staging)
        } finally {
            Files.deleteIfExists(staging)
        }
        return path
    }

    fun read(path: Path): CliEndpoint {
        val before = checkPrivate(path, false)
        require(before.size() <= MAX_ENDPOINT_BYTES) { "CLI接続情報が大きすぎます。" }
        val properties = Properties()
        FileChannel.open(path, READ, NOFOLLOW_LINKS).use {
            val bytes = Channels.newInputStream(it).readNBytes(MAX_ENDPOINT_BYTES + 1)
            require(bytes.size <= MAX_ENDPOINT_BYTES) { "CLI接続情報が大きすぎます。" }
            properties.load(bytes.inputStream())
        }
        require(checkPrivate(path, false).fileKey() == before.fileKey()) { "CLI接続情報が変更されました。" }
        require(properties.stringPropertyNames() == setOf("protocol", "id", "project", "build", "port", "token", "pid", "started"))
        require(properties.getProperty("protocol") == ReplayCliProtocol.VERSION.toString()) { "CLI通信の版が異なります。" }
        val id = properties.getProperty("id")
        require(runCatching { UUID.fromString(id).toString() == id }.getOrDefault(false) && path.fileName.toString() == "$id.properties") {
            "CLI接続情報のIDが不正です。"
        }
        val project = properties.getProperty("project")
        require(project.length <= 8192 && runCatching { Path.of(project).isAbsolute }.getOrDefault(false)) { "CLI接続情報のprojectが不正です。" }
        val build = properties.getProperty("build")
        require(build.length in 1..256)
        val port = properties.getProperty("port").toIntOrNull()
        require(port != null && port in 1..65535) { "CLI接続情報のportが不正です。" }
        val token = properties.getProperty("token")
        require(Regex("[0-9a-f]{64}").matches(token))
        val pid = properties.getProperty("pid").toLongOrNull()
        require(pid != null && pid > 0) { "CLI接続情報のprocessが不正です。" }
        val started = runCatching { Instant.parse(properties.getProperty("started")) }.getOrNull()
        require(started != null) { "CLI接続情報のprocess開始時刻が不正です。" }
        return CliEndpoint(id, project, build, port, HexFormat.of().parseHex(token), pid, started)
    }

    fun select(directory: Path, project: Path?, build: String): CliEndpoint {
        if (!Files.exists(directory, NOFOLLOW_LINKS)) error("起動済みのAndroid Studioで対象projectを開いてください。")
        checkPrivate(directory, true)
        val target = project?.toRealPath()?.toString()
        val candidates = Files.newDirectoryStream(directory, "*.properties").use { files ->
            val paths = files.take(129).toList()
            require(paths.size <= 128) { "CLI接続情報が多すぎます。停止済みIDEの接続情報を確認してください。" }
            paths.map(::read).filter { (target == null || it.project == target) && it.isLive() }
        }
        require(candidates.isNotEmpty()) { "対象projectのCLI接続先がありません。Android Studioでprojectとプラグインを確認してください。" }
        require(candidates.size == 1) { "対象が複数あります。--projectでprojectを指定し、同じprojectの別IDEを閉じてください。" }
        return candidates.single().also {
            require(it.build == build) { "起動中プラグインとCLIの版が異なります。IDEを通常再起動し、同じ配布物のCLIを使ってください。" }
        }
    }
}

/**
 * Owns one project's loopback endpoint and bounded request workers; [execute] runs off EDT.
 * [close] requests shutdown without waiting. Observe [termination] for worker exit and descriptor
 * cleanup; exceptional completion does not confirm cleanup. A closed connection
 * does not establish the outcome of an already dispatched capture/save operation.
 */
internal class ReplayCliServer(
    private val directory: Path,
    project: Path,
    build: String,
    private val execute: (CliRequest) -> CliResponse,
) : AutoCloseable {
    private val closed = AtomicBoolean()
    val termination = CompletableFuture<Void>()
    private val sockets = ConcurrentHashMap.newKeySet<Socket>()
    private val workers = ThreadPoolExecutor(0, 4, 10, TimeUnit.SECONDS, SynchronousQueue(), { task ->
        Thread(task, "replay-cli-request").apply { isDaemon = true }
    })
    private val deadlines = ScheduledThreadPoolExecutor(1) { task ->
        Thread(task, "replay-cli-deadline").apply { isDaemon = true }
    }.apply { removeOnCancelPolicy = true }
    private val server = ServerSocket()
    val endpoint: CliEndpoint
    private val path: Path
    private val fileKey: Any?

    init {
        try {
            ReplayCliRegistry.prepare(directory)
            server.bind(InetSocketAddress(InetAddress.getByName("127.0.0.1"), 0), 4)
            val process = ProcessHandle.current()
            endpoint = CliEndpoint(UUID.randomUUID().toString(), project.toRealPath().toString(), build, server.localPort,
                ReplayCliProtocol.secret(), process.pid(), process.info().startInstant().orElseThrow())
            path = ReplayCliRegistry.publish(directory, endpoint)
            fileKey = ReplayCliRegistry.checkPrivate(path, false).fileKey()
        } catch (failure: Exception) {
            server.close()
            workers.shutdownNow()
            deadlines.shutdownNow()
            throw failure
        }
        Thread(::accept, "replay-cli-accept").apply { isDaemon = true; start() }
    }

    private fun accept() {
        try {
            while (!closed.get()) {
                val socket = try { server.accept() } catch (_: java.io.IOException) { break }
                sockets += socket
                if (closed.get()) { sockets -= socket; socket.close(); break }
                try {
                    workers.execute { serve(socket) }
                } catch (_: RejectedExecutionException) {
                    sockets -= socket
                    socket.close()
                }
            }
        } finally {
            runCatching {
                close()
                check(workers.awaitTermination(5, TimeUnit.SECONDS) && deadlines.awaitTermination(5, TimeUnit.SECONDS)) {
                    "CLI操作の終了を確認できません。"
                }
                ReplayCliRegistry.checkPrivate(directory, true)
                if (Files.exists(path, NOFOLLOW_LINKS) && ReplayCliRegistry.checkPrivate(path, false).fileKey() == fileKey &&
                    ReplayCliRegistry.read(path).id == endpoint.id) Files.delete(path)
            }.fold({ termination.complete(null) }, termination::completeExceptionally)
        }
    }

    private fun serve(socket: Socket) {
        val reading = AtomicBoolean(true)
        val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(ReplayCliProtocol.HANDSHAKE_TIMEOUT_MS.toLong())
        // A trickling local client must not hold a worker beyond the whole request deadline.
        val timeout = try {
            deadlines.schedule({
                if (reading.compareAndSet(true, false)) runCatching { socket.close() }
            }, ReplayCliProtocol.HANDSHAKE_TIMEOUT_MS.toLong(), TimeUnit.MILLISECONDS)
        } catch (_: RejectedExecutionException) {
            sockets -= socket
            socket.close()
            return
        }
        try {
            socket.use {
                socket.soTimeout = ReplayCliProtocol.HANDSHAKE_TIMEOUT_MS
                val input = DataInputStream(socket.getInputStream())
                val output = DataOutputStream(socket.getOutputStream())
                ReplayCliProtocol.authenticateServer(input, output, endpoint.token)
                ReplayCliProtocol.writeText(output, endpoint.id)
                ReplayCliProtocol.writeText(output, endpoint.project)
                ReplayCliProtocol.writeText(output, endpoint.build)
                output.flush()
                val request = ReplayCliProtocol.readRequest(input)
                if (!reading.compareAndSet(true, false) || System.nanoTime() - deadline >= 0 || closed.get()) return
                timeout.cancel(false)
                val result = execute(request)
                ReplayCliProtocol.writeResponse(output, result)
            }
        } catch (_: Exception) {
            // No raw request/token/path diagnostics; a disconnected client must not retry a mutation.
        } finally {
            reading.set(false)
            timeout.cancel(false)
            sockets -= socket
        }
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        try {
            server.close()
        } finally {
            sockets.forEach { runCatching { it.close() } }
            workers.shutdownNow()
            deadlines.shutdownNow()
        }
    }
}
