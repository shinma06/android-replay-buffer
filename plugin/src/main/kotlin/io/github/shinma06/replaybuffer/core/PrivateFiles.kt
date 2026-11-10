package io.github.shinma06.replaybuffer.core

import java.nio.channels.FileChannel
import java.nio.channels.FileLock
import java.nio.file.FileSystems
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.OpenOption
import java.nio.file.Path
import java.nio.file.attribute.FileAttributeView
import java.nio.file.attribute.UserPrincipal

/** OS-specific half of [PrivateFiles]: one implementation per OS, chosen by [HostPlatform]. */
internal interface OwnerOnlyFiles {
    /** Atomically replaces [target] with [source] with rename(2) semantics on every OS. */
    fun replace(source: Path, target: Path)

    fun createDirectory(path: Path): Path

    fun openNew(path: Path, options: Set<OpenOption>): FileChannel

    /** Replaces the permissions of an existing item, without following a link. */
    fun restrict(path: Path, directory: Boolean)

    fun isPrivate(path: Path, directory: Boolean): Boolean

    /** True when [path] is owned by the current user and only that user (or the OS administrators) can modify it. */
    fun isOwnedWithoutSharedWrite(path: Path): Boolean

    /** Permissions as persisted in local ownership records: POSIX keeps the established enum-name list, Windows the ACL text. */
    fun recordedPermissions(path: Path): String

    /** Text for messages and identity comparison: the POSIX mode, or the ACL entries. */
    fun describe(path: Path): String

    /** Stable identity of the item at [path]. */
    fun fileKey(path: Path): Any
}

/**
 * Owner-only files with one meaning on every OS: mode 700/600 where POSIX permissions exist, and on Windows
 * an ACL whose every entry allows the current user only. SYSTEM and Administrators play root's part on Windows:
 * like root on POSIX, their access to a parent does not make it shared.
 */
internal object PrivateFiles : OwnerOnlyFiles by HostPlatform.privateFiles {
    val owner: UserPrincipal by lazy {
        FileSystems.getDefault().userPrincipalLookupService.lookupPrincipalByName(System.getProperty("user.name"))
    }

    /**
     * Exclusive owner lock on one byte past any record data. Windows byte-range locks are mandatory, so a whole-file lock
     * would block other handles from reading the record; this byte still overlaps older whole-file locks.
     */
    fun tryOwnerLock(channel: FileChannel): FileLock? = channel.tryLock(Long.MAX_VALUE - 1, 1, false)
}

internal inline fun <reified T : FileAttributeView> attributeView(path: Path): T =
    Files.getFileAttributeView(path, T::class.java, NOFOLLOW_LINKS)
        ?: throw UnsupportedOperationException("この環境ではファイルの権限を確認できません")
