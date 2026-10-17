package io.github.shinma06.replaybuffer.core

import com.sun.jna.Library
import com.sun.jna.Memory
import com.sun.jna.Native
import com.sun.jna.NativeLibrary
import com.sun.jna.Pointer
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.FileAlreadyExistsException
import java.nio.file.Path
import java.nio.file.StandardOpenOption.READ
import java.nio.file.StandardOpenOption.WRITE

internal interface SaveFileDescriptors : Library {
    fun open(path: String, flags: Int, vararg mode: Any): Int
    fun openat(parent: Int, name: String, flags: Int, vararg mode: Any): Int
    fun mkdirat(parent: Int, name: String, mode: Int): Int
    fun close(descriptor: Int): Int
    fun geteuid(): Int
    fun fdopendir(descriptor: Int): Pointer?
    fun closedir(directory: Pointer): Int
    fun acl_get_fd_np(descriptor: Int, type: Int): Pointer?
    fun acl_get_entry(acl: Pointer, entry: Int, result: Pointer): Int
    fun acl_free(acl: Pointer): Int
}

private val mac = System.getProperty("os.name") == "Mac OS X"
private val arm = System.getProperty("os.arch") in listOf("aarch64", "arm64")
private val descriptors by lazy {
    check(System.getProperty("os.name") in listOf("Mac OS X", "Linux") && Native.POINTER_SIZE == 8 &&
        System.getProperty("os.arch") in listOf("aarch64", "arm64", "amd64", "x86_64")) {
        "この環境は保存出力の実体確認に対応していません"
    }
    Native.load("c", SaveFileDescriptors::class.java, mapOf(Library.OPTION_STRING_ENCODING to "UTF-8"))
}
// O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC, using each platform ABI.
private val directoryFlags get() = if (mac) 0x01100100 else 0x000b0000

private fun childName(name: String) {
    require(name.isNotEmpty() && name != "." && name != ".." && '/' !in name && '\u0000' !in name)
}

private fun checkedDescriptor(descriptor: Int, name: String): Int {
    if (descriptor < 0) {
        if (Native.getLastError() == 17) throw FileAlreadyExistsException(name)
        throw IOException("保存出力を開けません（errno=${Native.getLastError()}）")
    }
    return descriptor
}

private fun <T> stat(descriptor: Int, block: (Memory, Int) -> T): T = Memory(256).use { value ->
    // Fixed Darwin/Linux 64-bit layouts; /dev/fd metadata substitutes the device ID on macOS.
    val symbol = if (mac && !arm) "fstat\$INODE64" else "fstat"
    if (NativeLibrary.getInstance("c").getFunction(symbol).invokeInt(arrayOf(descriptor, value)) != 0)
        throw IOException("保存出力の実体を確認できません")
    block(value, if (mac) value.getShort(4).toInt() and 0xffff else value.getInt(if (arm) 16 else 24))
}

private fun descriptorKey(descriptor: Int, type: Int): Pair<Long, Long> = stat(descriptor) { value, mode ->
    check(mode and 0xf000 == type) { "保存出力の実体を確認できません" }
    (if (mac) value.getInt(0).toLong() and 0xffffffffL else value.getLong(0)) to value.getLong(8)
}

internal class SaveOutputDirectory private constructor(internal val descriptor: Int) : AutoCloseable {
    private var closed = false
    val key = descriptorKey(descriptor, 0x4000)

    fun createDirectory(name: String): SaveOutputDirectory {
        check(!closed)
        childName(name)
        checkedDescriptor(descriptors.mkdirat(descriptor, name, 0x1c0), name)
        return openChild(name)
    }

    fun openChild(name: String): SaveOutputDirectory {
        check(!closed)
        childName(name)
        return fromDescriptor(checkedDescriptor(descriptors.openat(descriptor, name, directoryFlags), name))
    }

    fun createFile(name: String): SaveOutputFile {
        check(!closed)
        childName(name)
        return SaveOutputFile.fromDescriptor(checkedDescriptor(descriptors.openat(descriptor, name,
            if (mac) 0x01000b02 else 0x000a00c2, 0x180), name))
    }

    fun verifyFile(name: String, output: SaveOutputFile) {
        check(!closed)
        childName(name)
        val current = checkedDescriptor(descriptors.openat(descriptor, name, if (mac) 0x01000104 else 0x000a0800), name)
        try { check(descriptorKey(current, 0x8000) == output.key) { "保存出力の実体が変更されています" } }
        finally { descriptors.close(current) }
    }

    fun requirePrivate() {
        stat(descriptor) { value, mode ->
            val uid = value.getInt(if (mac) 16 else if (arm) 24 else 28)
            check(mode and 0xfff == 0x1c0 && uid == descriptors.geteuid()) { "未完成フォルダーのアクセス権を確認できません" }
        }
        if (mac) {
            Native.setLastError(0)
            val acl = descriptors.acl_get_fd_np(descriptor, 0x100)
            if (acl == null) {
                // Darwin filesec_get_property reports ENOENT for an absent ACL on this held descriptor.
                if (Native.getLastError() == 2) return
                throw IOException("未完成フォルダーのACLを確認できません")
            }
            try { Memory(Native.POINTER_SIZE.toLong()).use { entry ->
                Native.setLastError(0)
                check(descriptors.acl_get_entry(acl, 0, entry) == -1 && Native.getLastError() == 22) {
                    "未完成フォルダーに継承されたACLがあります"
                }
            } } finally { descriptors.acl_free(acl) }
        }
    }

    fun requireEntries(outputs: Map<String, SaveOutputFile>) {
        requirePrivate()
        check(entries() == outputs.keys) { "未完成フォルダーに所有を確認できない出力があります" }
        outputs.forEach { (name, output) -> verifyFile(name, output) }
    }

    fun entries(): Set<String> {
        check(!closed)
        val duplicate = checkedDescriptor(descriptors.openat(descriptor, ".", directoryFlags), ".")
        val directory = descriptors.fdopendir(duplicate) ?: run {
            descriptors.close(duplicate)
            throw IOException("未完成フォルダーの内容を確認できません")
        }
        try {
            val names = linkedSetOf<String>()
            val symbol = if (mac && !arm) "readdir\$INODE64" else "readdir"
            val read = NativeLibrary.getInstance("c").getFunction(symbol)
            while (true) {
                Native.setLastError(0)
                val entry = read.invokePointer(arrayOf(directory))
                if (entry == null) {
                    if (Native.getLastError() != 0) throw IOException("未完成フォルダーの内容を確認できません")
                    return names
                }
                val name = entry.getString(if (mac) 21 else 19, "UTF-8")
                if (name != "." && name != "..") check(names.add(name)) { "未完成フォルダーの内容が変更されています" }
            }
        } finally { descriptors.closedir(directory) }
    }

    override fun close() {
        if (!closed) { closed = true; descriptors.close(descriptor) }
    }

    companion object {
        fun open(path: Path): SaveOutputDirectory {
            require(path.isAbsolute && path == path.normalize())
            var current = fromDescriptor(checkedDescriptor(descriptors.open("/", directoryFlags), "/"))
            try {
                for (component in path) {
                    val next = current.openChild(component.toString())
                    current.close()
                    current = next
                }
                return current
            } catch (e: Throwable) { current.close(); throw e }
        }

        private fun fromDescriptor(descriptor: Int): SaveOutputDirectory = try { SaveOutputDirectory(descriptor) }
            catch (e: Throwable) { descriptors.close(descriptor); throw e }
    }
}

/** Keep the creation descriptor alive; all file IO uses that object. */
internal class SaveOutputFile private constructor(private val descriptor: Int) : AutoCloseable {
    private var closed = false
    val key = descriptorKey(descriptor, 0x8000)

    fun channel(): FileChannel {
        check(!closed)
        return FileChannel.open(Path.of(if (mac) "/dev/fd" else "/proc/self/fd", descriptor.toString()), READ, WRITE)
    }

    fun hash(): String {
        val digest = java.security.MessageDigest.getInstance("SHA-256")
        channel().use { channel ->
            channel.position(0)
            val bytes = ByteBuffer.allocate(8192)
            while (channel.read(bytes) >= 0) { bytes.flip(); digest.update(bytes); bytes.clear() }
        }
        return java.util.HexFormat.of().formatHex(digest.digest())
    }

    override fun close() {
        if (!closed) { closed = true; descriptors.close(descriptor) }
    }

    companion object {
        fun fromDescriptor(descriptor: Int): SaveOutputFile = try { SaveOutputFile(descriptor) }
            catch (e: Throwable) { descriptors.close(descriptor); throw e }
    }
}
