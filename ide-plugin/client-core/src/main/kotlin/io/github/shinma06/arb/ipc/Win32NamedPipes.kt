package io.github.shinma06.arb.ipc

import java.io.IOException
import java.lang.foreign.Arena
import java.lang.foreign.FunctionDescriptor
import java.lang.foreign.Linker
import java.lang.foreign.MemoryLayout
import java.lang.foreign.MemorySegment
import java.lang.foreign.SymbolLookup
import java.lang.foreign.ValueLayout
import java.lang.invoke.MethodHandle
import java.nio.charset.StandardCharsets
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.locks.ReentrantReadWriteLock

/**
 * Win32 Named Pipe client built on the standard Java Foreign Function & Memory API
 * (`java.lang.foreign`, final since JDK 22) — zero JNI, zero JNA, zero third-party code.
 *
 * Only reachable through [LocalIpcTransport.connect] when [HostOs] is [HostOs.WINDOWS]. Mirrors
 * `arb-os/src/ipc.rs` (`Stream::connect`):
 * - `CreateFileW(..., SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)` so the server can at most
 *   *identify* (never impersonate) the IDE user;
 * - `GetKernelObjectSecurity(OWNER_SECURITY_INFORMATION)` + `GetSecurityDescriptorOwner` to read the
 *   pipe owner SID, compared by [OwnerSidPipeVerifier] against the process token's `TokenUser` SID.
 *
 * Any failure to bind the Win32 functions (e.g. a JVM without FFM) surfaces as [IOException], so the
 * transport fails closed instead of talking to an unverified pipe.
 */
internal object Win32NamedPipes : WindowsPipeOpener {
    private const val GENERIC_READ = 0x8000_0000.toInt()
    private const val GENERIC_WRITE = 0x4000_0000
    private const val OPEN_EXISTING = 3
    private const val SECURITY_SQOS_PRESENT = 0x0010_0000
    private const val SECURITY_IDENTIFICATION = 0x0001_0000 // SecurityIdentification << 16
    private const val OWNER_SECURITY_INFORMATION = 0x0000_0001
    private const val TOKEN_QUERY = 0x0008
    private const val TOKEN_USER_CLASS = 1
    private const val ERROR_FILE_NOT_FOUND = 2
    private const val ERROR_BROKEN_PIPE = 109
    private const val ERROR_INSUFFICIENT_BUFFER = 122
    private const val ERROR_PIPE_BUSY = 231
    private const val ERROR_OPERATION_ABORTED = 995
    private const val MAX_SID_BYTES = 68L // SECURITY_MAX_SID_SIZE

    /** Lazily bound Win32 entry points (only ever initialized on Windows). */
    private class Api {
        val linker: Linker = Linker.nativeLinker()
        private val kernel32: SymbolLookup = SymbolLookup.libraryLookup("kernel32.dll", Arena.global())
        private val advapi32: SymbolLookup = SymbolLookup.libraryLookup("advapi32.dll", Arena.global())
        val captureLayout = Linker.Option.captureStateLayout()
        val lastErrorOffset: Long = captureLayout.byteOffset(MemoryLayout.PathElement.groupElement("GetLastError"))
        private val capture = Linker.Option.captureCallState("GetLastError")

        private fun bind(lookup: SymbolLookup, name: String, fd: FunctionDescriptor, captureError: Boolean): MethodHandle {
            val symbol = lookup.find(name).orElseThrow { IOException("Win32 symbol $name not found") }
            return if (captureError) linker.downcallHandle(symbol, fd, capture) else linker.downcallHandle(symbol, fd)
        }

        private val addr = ValueLayout.ADDRESS
        private val i32 = ValueLayout.JAVA_INT

        val createFileW = bind(kernel32, "CreateFileW", FunctionDescriptor.of(addr, addr, i32, i32, addr, i32, i32, addr), true)
        val readFile = bind(kernel32, "ReadFile", FunctionDescriptor.of(i32, addr, addr, i32, addr, addr), true)
        val writeFile = bind(kernel32, "WriteFile", FunctionDescriptor.of(i32, addr, addr, i32, addr, addr), true)
        val closeHandle = bind(kernel32, "CloseHandle", FunctionDescriptor.of(i32, addr), false)
        val cancelIoEx = bind(kernel32, "CancelIoEx", FunctionDescriptor.of(i32, addr, addr), false)
        val getCurrentProcess = bind(kernel32, "GetCurrentProcess", FunctionDescriptor.of(addr), false)
        val openProcessToken = bind(advapi32, "OpenProcessToken", FunctionDescriptor.of(i32, addr, i32, addr), true)
        val getTokenInformation = bind(advapi32, "GetTokenInformation", FunctionDescriptor.of(i32, addr, i32, addr, i32, addr), true)
        val getKernelObjectSecurity =
            bind(advapi32, "GetKernelObjectSecurity", FunctionDescriptor.of(i32, addr, i32, addr, i32, addr), true)
        val getSecurityDescriptorOwner =
            bind(advapi32, "GetSecurityDescriptorOwner", FunctionDescriptor.of(i32, addr, addr, addr), false)
        val getLengthSid = bind(advapi32, "GetLengthSid", FunctionDescriptor.of(i32, addr), false)
        val isValidSid = bind(advapi32, "IsValidSid", FunctionDescriptor.of(i32, addr), false)

        fun lastError(state: MemorySegment): Int = state.get(ValueLayout.JAVA_INT, lastErrorOffset)
    }

    @Volatile
    private var api: Api? = null

    private fun api(): Api {
        api?.let { return it }
        synchronized(this) {
            api?.let { return it }
            val bound = try {
                Api()
            } catch (e: IOException) {
                throw e
            } catch (e: Throwable) {
                throw IOException("Windows named pipe security APIs are unavailable on this JVM: $e", e)
            }
            api = bound
            return bound
        }
    }

    private fun call(handle: MethodHandle, vararg args: Any?): Any? = handle.invokeWithArguments(*args)

    private fun Any?.asInt(): Int = this as Int

    private fun Any?.asSegment(): MemorySegment = this as MemorySegment

    /** The process token user's SID bytes (`TokenUser`), the identity `arbd` sets as pipe owner. */
    @Throws(IOException::class)
    fun currentUserSid(): ByteArray {
        val a = api()
        Arena.ofConfined().use { arena ->
            val state = arena.allocate(a.captureLayout)
            val tokenOut = arena.allocate(ValueLayout.ADDRESS)
            val process = call(a.getCurrentProcess).asSegment()
            if (call(a.openProcessToken, state, process, TOKEN_QUERY, tokenOut).asInt() == 0) {
                throw IOException("OpenProcessToken failed (error ${a.lastError(state)})")
            }
            val token = tokenOut.get(ValueLayout.ADDRESS, 0)
            try {
                val needed = arena.allocate(ValueLayout.JAVA_INT)
                call(a.getTokenInformation, state, token, TOKEN_USER_CLASS, MemorySegment.NULL, 0, needed)
                val size = needed.get(ValueLayout.JAVA_INT, 0)
                if (size <= 0) {
                    throw IOException("GetTokenInformation(TokenUser) size query failed (error ${a.lastError(state)})")
                }
                val buf = arena.allocate(size.toLong(), 8)
                if (call(a.getTokenInformation, state, token, TOKEN_USER_CLASS, buf, size, needed).asInt() == 0) {
                    throw IOException("GetTokenInformation(TokenUser) failed (error ${a.lastError(state)})")
                }
                // TOKEN_USER { SID_AND_ATTRIBUTES User { PSID Sid; DWORD Attributes } }
                return copySid(a, buf.get(ValueLayout.ADDRESS, 0), "token user")
            } finally {
                call(a.closeHandle, token)
            }
        }
    }

    private fun copySid(a: Api, sid: MemorySegment, what: String): ByteArray {
        if (sid.address() == 0L || call(a.isValidSid, sid).asInt() == 0) {
            throw IOException("Invalid $what SID")
        }
        val len = call(a.getLengthSid, sid).asInt().toLong()
        if (len <= 0L || len > MAX_SID_BYTES) throw IOException("Unexpected $what SID length $len")
        return sid.reinterpret(len).toArray(ValueLayout.JAVA_BYTE)
    }

    /** Verifier comparing the pipe owner SID against [currentUserSid]. */
    val ownerVerifier: WindowsPipeSecurityVerifier = OwnerSidPipeVerifier { currentUserSid() }

    override fun open(path: String): WindowsPipeHandle {
        val a = api()
        Arena.ofConfined().use { arena ->
            val state = arena.allocate(a.captureLayout)
            val name = arena.allocateFrom(path, StandardCharsets.UTF_16LE)
            val handle = call(
                a.createFileW,
                state,
                name,
                GENERIC_READ or GENERIC_WRITE,
                0,
                MemorySegment.NULL,
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT or SECURITY_IDENTIFICATION,
                MemorySegment.NULL,
            ).asSegment()
            if (handle.address() == -1L || handle.address() == 0L) {
                when (val err = a.lastError(state)) {
                    ERROR_PIPE_BUSY -> throw WindowsPipeBusyException("All pipe instances are busy: $path")
                    ERROR_FILE_NOT_FOUND -> throw IOException("Windows named pipe not found: $path (error $err)")
                    else -> throw IOException("CreateFileW failed for $path (error $err)")
                }
            }
            return Win32PipeHandle(a, MemorySegment.ofAddress(handle.address()))
        }
    }

    /** A synchronous (non-overlapped) client pipe handle; one request/response in flight at a time. */
    private class Win32PipeHandle(
        private val a: Api,
        private val handle: MemorySegment,
    ) : WindowsPipeHandle {
        private val closed = AtomicBoolean(false)
        private val ioLock = ReentrantReadWriteLock()

        private inline fun <T> withOpenHandle(block: () -> T): T {
            val lock = ioLock.readLock()
            lock.lock()
            try {
                if (closed.get()) throw IOException("Windows named pipe is closed")
                return block()
            } finally {
                lock.unlock()
            }
        }

        override fun serverOwnerSid(): ByteArray = withOpenHandle {
            Arena.ofConfined().use { arena ->
                val state = arena.allocate(a.captureLayout)
                val needed = arena.allocate(ValueLayout.JAVA_INT)
                call(a.getKernelObjectSecurity, state, handle, OWNER_SECURITY_INFORMATION, MemorySegment.NULL, 0, needed)
                val size = needed.get(ValueLayout.JAVA_INT, 0)
                if (size <= 0) {
                    throw IOException("GetKernelObjectSecurity size query failed (error ${a.lastError(state)})")
                }
                val sd = arena.allocate(size.toLong(), 8)
                if (call(a.getKernelObjectSecurity, state, handle, OWNER_SECURITY_INFORMATION, sd, size, needed)
                        .asInt() == 0
                ) {
                    val err = a.lastError(state)
                    val hint = if (err == ERROR_INSUFFICIENT_BUFFER) " (descriptor grew)" else ""
                    throw IOException("GetKernelObjectSecurity failed (error $err)$hint")
                }
                val ownerOut = arena.allocate(ValueLayout.ADDRESS)
                val defaulted = arena.allocate(ValueLayout.JAVA_INT)
                if (call(a.getSecurityDescriptorOwner, sd, ownerOut, defaulted).asInt() == 0) {
                    throw IOException("GetSecurityDescriptorOwner failed")
                }
                copySid(a, ownerOut.get(ValueLayout.ADDRESS, 0), "pipe owner")
            }
        }

        override fun read(b: ByteArray, off: Int, len: Int): Int {
            if (len == 0) return 0
            return withOpenHandle {
                Arena.ofConfined().use { arena ->
                    val state = arena.allocate(a.captureLayout)
                    val buf = arena.allocate(len.toLong())
                    val got = arena.allocate(ValueLayout.JAVA_INT)
                    var result = 0
                    while (result == 0) {
                        val ok = call(a.readFile, state, handle, buf, len, got, MemorySegment.NULL).asInt()
                        if (ok == 0) {
                            val err = a.lastError(state)
                            val cleanEof = err == ERROR_BROKEN_PIPE || (err == ERROR_OPERATION_ABORTED && closed.get())
                            if (!cleanEof) throw IOException("ReadFile failed on Windows named pipe (error $err)")
                            result = -1
                        } else {
                            // Byte-mode pipes may complete a zero-length read; keep waiting for data.
                            val n = got.get(ValueLayout.JAVA_INT, 0)
                            if (n > 0) {
                                MemorySegment.copy(buf, ValueLayout.JAVA_BYTE, 0, b, off, n)
                                result = n
                            }
                        }
                    }
                    result
                }
            }
        }

        override fun write(b: ByteArray, off: Int, len: Int) {
            if (len == 0) return
            withOpenHandle {
                Arena.ofConfined().use { arena ->
                    val state = arena.allocate(a.captureLayout)
                    val buf = arena.allocate(len.toLong())
                    MemorySegment.copy(b, off, buf, ValueLayout.JAVA_BYTE, 0, len)
                    val wrote = arena.allocate(ValueLayout.JAVA_INT)
                    var done = 0
                    while (done < len) {
                        val chunk = buf.asSlice(done.toLong())
                        val ok = call(a.writeFile, state, handle, chunk, len - done, wrote, MemorySegment.NULL).asInt()
                        if (ok == 0) {
                            throw IOException("WriteFile failed on Windows named pipe (error ${a.lastError(state)})")
                        }
                        val n = wrote.get(ValueLayout.JAVA_INT, 0)
                        if (n <= 0) throw IOException("WriteFile wrote no bytes to Windows named pipe")
                        done += n
                    }
                }
            }
        }

        override fun close() {
            if (!closed.compareAndSet(false, true)) return
            val writeLock = ioLock.writeLock()
            // Unblock a ReadFile parked on another thread (e.g. the RPC read executor) before
            // closing, so the handle is never closed underneath an in-flight call.
            repeat(CLOSE_CANCEL_ATTEMPTS) {
                runCatching { call(a.cancelIoEx, handle, MemorySegment.NULL) }
                if (writeLock.tryLock(CLOSE_CANCEL_WAIT_MS, TimeUnit.MILLISECONDS)) {
                    try {
                        call(a.closeHandle, handle)
                    } finally {
                        writeLock.unlock()
                    }
                    return
                }
            }
            // Last resort (matches the previous RandomAccessFile behaviour): close regardless.
            runCatching { call(a.closeHandle, handle) }
        }

        companion object {
            private const val CLOSE_CANCEL_ATTEMPTS = 50
            private const val CLOSE_CANCEL_WAIT_MS = 20L
        }
    }
}
