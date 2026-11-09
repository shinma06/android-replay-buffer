package io.github.shinma06.replaybuffer.core

import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.OpenOption
import java.nio.file.Path
import java.nio.file.StandardCopyOption.ATOMIC_MOVE
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.nio.file.attribute.BasicFileAttributes
import java.nio.file.attribute.PosixFileAttributeView
import java.nio.file.attribute.PosixFilePermission
import java.nio.file.attribute.PosixFilePermissions

/** Linux and macOS owner-only files: mode 700/600 from creation. */
internal object PosixPrivateFiles : OwnerOnlyFiles {
    private val directoryMode = PosixFilePermissions.fromString("rwx------")
    private val fileMode = PosixFilePermissions.fromString("rw-------")
    private val sharedWrite = setOf(PosixFilePermission.GROUP_WRITE, PosixFilePermission.OTHERS_WRITE)

    private fun mode(directory: Boolean) = if (directory) directoryMode else fileMode

    override fun replace(source: Path, target: Path) {
        Files.move(source, target, ATOMIC_MOVE, REPLACE_EXISTING)
    }

    override fun createDirectory(path: Path): Path {
        Files.createDirectory(path, PosixFilePermissions.asFileAttribute(directoryMode))
        return path
    }

    override fun openNew(path: Path, options: Set<OpenOption>): FileChannel =
        FileChannel.open(path, options, PosixFilePermissions.asFileAttribute(fileMode))

    override fun restrict(path: Path, directory: Boolean) {
        attributeView<PosixFileAttributeView>(path).setPermissions(mode(directory))
    }

    override fun isPrivate(path: Path, directory: Boolean): Boolean = attributeView<PosixFileAttributeView>(path).readAttributes().let {
        it.owner() == PrivateFiles.owner && it.permissions() == mode(directory)
    }

    override fun isOwnedWithoutSharedWrite(path: Path): Boolean =
        attributeView<PosixFileAttributeView>(path).readAttributes().let { attributes ->
            attributes.owner() == PrivateFiles.owner && attributes.permissions().none { it in sharedWrite }
        }

    override fun recordedPermissions(path: Path): String =
        attributeView<PosixFileAttributeView>(path).readAttributes().permissions().sorted().joinToString(",")

    override fun describe(path: Path): String =
        PosixFilePermissions.toString(attributeView<PosixFileAttributeView>(path).readAttributes().permissions())

    override fun fileKey(path: Path): Any =
        Files.readAttributes(path, BasicFileAttributes::class.java, NOFOLLOW_LINKS).fileKey() ?: error("ファイルの実体を確認できません")
}
