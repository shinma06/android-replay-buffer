package io.github.shinma06.replaybuffer.core

import com.sun.jna.Library
import com.sun.jna.Memory
import com.sun.jna.Native
import com.sun.jna.NativeLibrary
import com.sun.jna.Pointer
import java.io.IOException
import java.nio.channels.FileChannel
import java.nio.file.FileAlreadyExistsException
import java.nio.file.OpenOption
import java.nio.file.Path

internal interface SaveFileDescriptors : Library {
    fun open(path: String, flags: Int, vararg mode: Any): Int
    fun openat(parent: Int, name: String, flags: Int, vararg mode: Any): Int
    fun mkdirat(parent: Int, name: String, mode: Int): Int
    fun unlinkat(parent: Int, name: String, flags: Int): Int
    fun fsync(descriptor: Int): Int
    fun close(descriptor: Int): Int
    fun geteuid(): Int
    fun fdopendir(descriptor: Int): Pointer?
    fun closedir(directory: Pointer): Int
    fun acl_get_fd_np(descriptor: Int, type: Int): Pointer?
    fun acl_get_entry(acl: Pointer, entry: Int, result: Pointer): Int
    fun acl_free(acl: Pointer): Int
}

/** Use the IDE's existing JNA, not another runtime dependency or a bundled native binary. */
internal interface ExclusiveRename : Library {
    fun renameatx_np(sourceParent: Int, source: String, destinationParent: Int, destination: String, flags: Int): Int
    fun renameat2(sourceParent: Int, source: String, destinationParent: Int, destination: String, flags: Int): Int
}

/** Linux and macOS save handles: libc descriptors via JNA, with each platform's flags and 64-bit struct layouts. */
internal class PosixSaveHandles(private val darwin: Boolean) : SaveHandles {
    private val arm = System.getProperty("os.arch") in listOf("aarch64", "arm64")
    private val descriptors by lazy {
        check(Native.POINTER_SIZE == 8 && System.getProperty("os.arch") in listOf("aarch64", "arm64", "amd64", "x86_64")) {
            "この環境は保存出力の実体確認に対応していません"
        }
        Native.load("c", SaveFileDescriptors::class.java, mapOf(Library.OPTION_STRING_ENCODING to "UTF-8"))
    }
    private val exclusiveRename by lazy {
        Native.load("c", ExclusiveRename::class.java, mapOf(Library.OPTION_STRING_ENCODING to "UTF-8"))
    }
    // O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC, using each platform ABI.
    private val directoryFlags = if (darwin) 0x01100100 else 0x000b0000
    private val readFlags = if (darwin) 0x01000104 else 0x000a0800
    private val createFlags = if (darwin) 0x01000b02 else 0x000a00c2
    private val removeDirectory = if (darwin) 0x80 else 0x200
    private val descriptorDirectory = if (darwin) "/dev/fd" else "/proc/self/fd"

    override val renamesDirectoryWithOpenFiles = true

    override fun openRoot(path: Path): Long = checkedDescriptor(descriptors.open("/", directoryFlags), "/").toLong()

    override fun openDirectory(parent: Long, name: String): Long =
        checkedDescriptor(descriptors.openat(parent.toInt(), name, directoryFlags), name).toLong()

    override fun createDirectory(parent: Long, name: String): Long {
        checkedDescriptor(descriptors.mkdirat(parent.toInt(), name, 0x1c0), name)
        return openDirectory(parent, name)
    }

    override fun openFile(parent: Long, name: String): Long =
        checkedDescriptor(descriptors.openat(parent.toInt(), name, readFlags), name).toLong()

    override fun createFile(parent: Long, name: String): Long =
        checkedDescriptor(descriptors.openat(parent.toInt(), name, createFlags, 0x180), name).toLong()

    override fun key(handle: Long, directory: Boolean): Pair<Long, Long> = stat(handle.toInt()) { value, mode ->
        check(mode and 0xf000 == (if (directory) 0x4000 else 0x8000)) { "保存出力の実体を確認できません" }
        (if (darwin) value.getInt(0).toLong() and 0xffffffffL else value.getLong(0)) to value.getLong(8)
    }

    override fun renameExclusive(sourceParent: Long, source: String, destinationParent: Long, destination: String) {
        // RENAME_EXCL (Darwin) / RENAME_NOREPLACE (Linux): fail instead of replacing any existing target.
        val result = try {
            if (darwin) exclusiveRename.renameatx_np(sourceParent.toInt(), source, destinationParent.toInt(), destination, 0x4)
            else exclusiveRename.renameat2(sourceParent.toInt(), source, destinationParent.toInt(), destination, 0x1)
        } catch (_: LinkageError) { error("原子・非上書き保存を利用できません") }
        check(result == 0) { "完成先の公開に失敗しました（既存出力・対応filesystem・アクセス権を確認してください）" }
    }

    override fun deleteFile(parent: Long, name: String) {
        check(descriptors.unlinkat(parent.toInt(), name, 0) == 0) { "保存したファイルを削除できません。アクセス権を確認してください。" }
    }

    override fun deleteEmptyDirectory(parent: Long, name: String) {
        check(descriptors.unlinkat(parent.toInt(), name, removeDirectory) == 0) { "削除用の空フォルダを片付けられません。" }
    }

    override fun flush(directory: Long) {
        check(descriptors.fsync(directory.toInt()) == 0) { "保存先への書込み完了を確認できません。" }
    }

    override fun entries(directory: Long): Set<String> {
        val duplicate = checkedDescriptor(descriptors.openat(directory.toInt(), ".", directoryFlags), ".")
        val stream = descriptors.fdopendir(duplicate) ?: run {
            descriptors.close(duplicate)
            throw IOException("未完成フォルダーの内容を確認できません")
        }
        try {
            val names = linkedSetOf<String>()
            val symbol = if (darwin && !arm) "readdir\$INODE64" else "readdir"
            val read = NativeLibrary.getInstance("c").getFunction(symbol)
            while (true) {
                Native.setLastError(0)
                val entry = read.invokePointer(arrayOf(stream))
                if (entry == null) {
                    if (Native.getLastError() != 0) throw IOException("未完成フォルダーの内容を確認できません")
                    return names
                }
                val name = entry.getString(if (darwin) 21 else 19, "UTF-8")
                if (name != "." && name != "..") check(names.add(name)) { "未完成フォルダーの内容が変更されています" }
            }
        } finally { descriptors.closedir(stream) }
    }

    override fun channel(handle: Long, options: Set<OpenOption>): FileChannel =
        FileChannel.open(Path.of(descriptorDirectory, handle.toString()), options)

    override fun requirePrivate(handle: Long, directory: Boolean) {
        val descriptor = handle.toInt()
        val permissions = if (directory) 0x1c0 else 0x180
        stat(descriptor) { value, mode ->
            val uid = value.getInt(if (darwin) 16 else if (arm) 24 else 28)
            check(mode and 0xfff == permissions && uid == descriptors.geteuid()) {
                "未完成フォルダーのアクセス権を確認できません"
            }
        }
        if (darwin) {
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

    override fun close(handle: Long) { descriptors.close(handle.toInt()) }

    private fun checkedDescriptor(descriptor: Int, name: String): Int {
        if (descriptor < 0) {
            if (Native.getLastError() == 17) throw FileAlreadyExistsException(name)
            throw IOException("保存出力を開けません（errno=${Native.getLastError()}）")
        }
        return descriptor
    }

    private fun <T> stat(descriptor: Int, block: (Memory, Int) -> T): T = Memory(256).use { value ->
        // Fixed Darwin/Linux 64-bit layouts; /dev/fd metadata substitutes the device ID on macOS.
        val symbol = if (darwin && !arm) "fstat\$INODE64" else "fstat"
        if (NativeLibrary.getInstance("c").getFunction(symbol).invokeInt(arrayOf(descriptor, value)) != 0)
            throw IOException("保存出力の実体を確認できません")
        block(value, if (darwin) value.getShort(4).toInt() and 0xffff else value.getInt(if (arm) 16 else 24))
    }
}
