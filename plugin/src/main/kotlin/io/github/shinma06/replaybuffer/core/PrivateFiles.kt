package io.github.shinma06.replaybuffer.core

import com.sun.jna.Library
import com.sun.jna.Native
import com.sun.jna.Pointer
import com.sun.jna.WString
import com.sun.jna.ptr.IntByReference
import com.sun.jna.ptr.PointerByReference
import java.nio.channels.FileChannel
import java.nio.file.FileSystems
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.OpenOption
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.nio.file.attribute.AclEntry
import java.nio.file.attribute.AclEntryFlag
import java.nio.file.attribute.AclEntryPermission
import java.nio.file.attribute.AclEntryType
import java.nio.file.attribute.AclFileAttributeView
import java.nio.file.attribute.FileAttribute
import java.nio.file.attribute.PosixFileAttributeView
import java.nio.file.attribute.PosixFilePermission
import java.nio.file.attribute.PosixFilePermissions
import java.nio.file.attribute.UserPrincipal

/**
 * Owner-only files with one meaning on every OS: mode 700/600 where POSIX permissions exist, and on Windows
 * an ACL whose every entry allows the current user only. SYSTEM and Administrators play root's part on Windows:
 * like root on POSIX, their access to a parent does not make it shared.
 */
internal object PrivateFiles {
    private val posix = "posix" in FileSystems.getDefault().supportedFileAttributeViews()
    private val directoryMode = PosixFilePermissions.fromString("rwx------")
    private val fileMode = PosixFilePermissions.fromString("rw-------")
    private val sharedWrite = setOf(PosixFilePermission.GROUP_WRITE, PosixFilePermission.OTHERS_WRITE)
    private val aclWrites = setOf(AclEntryPermission.WRITE_DATA, AclEntryPermission.APPEND_DATA, AclEntryPermission.DELETE_CHILD,
        AclEntryPermission.DELETE, AclEntryPermission.WRITE_ACL, AclEntryPermission.WRITE_OWNER)

    val owner: UserPrincipal by lazy {
        FileSystems.getDefault().userPrincipalLookupService.lookupPrincipalByName(System.getProperty("user.name"))
    }
    private val administrators: Set<UserPrincipal> by lazy { setOfNotNull(windowsPrincipal("S-1-5-18"), windowsPrincipal("S-1-5-32-544")) }

    fun attribute(directory: Boolean): FileAttribute<*> =
        if (posix) PosixFilePermissions.asFileAttribute(if (directory) directoryMode else fileMode)
        else object : FileAttribute<List<AclEntry>> {
            override fun name() = "acl:acl"
            override fun value() = ownerAcl(directory)
        }

    /** Atomically replaces [target] with [source] with rename(2) semantics on every OS. */
    fun replace(source: Path, target: Path) {
        if (posix) Files.move(source, target, StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
        else WindowsSaveHandles.replace(source, target)
    }

    fun createDirectory(path: Path): Path {
        Files.createDirectory(path, attribute(true))
        if (!posix) restrict(path, true)
        return path
    }

    fun openNew(path: Path, options: Set<OpenOption>): FileChannel {
        val channel = try { FileChannel.open(path, options, attribute(false)) } catch (denied: java.nio.file.AccessDeniedException) {
            // Windows reports CREATE_NEW over an existing directory as access denied; POSIX reports it as existing.
            if (!posix && Files.exists(path, NOFOLLOW_LINKS)) throw java.nio.file.FileAlreadyExistsException(path.toString()).apply { initCause(denied) }
            throw denied
        }
        try {
            if (!posix) restrict(path, false)
        } catch (failure: Exception) {
            channel.close()
            throw failure
        }
        return channel
    }

    /** Replaces the permissions of an existing item, without following a link. */
    fun restrict(path: Path, directory: Boolean) {
        if (posix) view<PosixFileAttributeView>(path).setPermissions(if (directory) directoryMode else fileMode)
        else view<AclFileAttributeView>(path).let { if (it.owner != owner) it.owner = owner; it.acl = ownerAcl(directory) }
    }

    /** Runs [block] with a native owner-only security descriptor, so Windows items are private from creation like mode 700/600. */
    fun <T> withWindowsDescriptor(directory: Boolean, block: (Pointer) -> T): T {
        val sid = ByteArray(68)
        check(WindowsAccounts.advapi.LookupAccountNameW(null, WString(owner.name), sid, IntByReference(sid.size), CharArray(256),
            IntByReference(256), IntByReference())) { "現在のユーザーを確認できません" }
        val text = PointerByReference()
        check(WindowsAccounts.advapi.ConvertSidToStringSidW(sid, text)) { "現在のユーザーを確認できません" }
        val user = try { text.value.getWideString(0) } finally { WindowsAccounts.kernel.LocalFree(text.value) }
        val descriptor = PointerByReference()
        check(WindowsAccounts.advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW(
            WString("O:${user}D:P(A;${if (directory) "OICI" else ""};FA;;;$user)"), 1, descriptor, null)) {
            "アクセス権を設定できません"
        }
        try { return block(descriptor.value) } finally { WindowsAccounts.kernel.LocalFree(descriptor.value) }
    }

    fun isPrivate(path: Path, directory: Boolean): Boolean =
        if (posix) view<PosixFileAttributeView>(path).readAttributes().let {
            it.owner() == owner && it.permissions() == if (directory) directoryMode else fileMode
        } else view<AclFileAttributeView>(path).let { acl ->
            acl.owner == owner && acl.acl.all { it.type() == AclEntryType.ALLOW && it.principal() == owner }
        }

    /** True when [path] is owned by the current user and only that user (or the OS administrators) can modify it. */
    fun isOwnedWithoutSharedWrite(path: Path): Boolean =
        if (posix) view<PosixFileAttributeView>(path).readAttributes().let { attributes ->
            attributes.owner() == owner && attributes.permissions().none { it in sharedWrite }
        } else view<AclFileAttributeView>(path).let { acl ->
            acl.owner == owner && acl.acl.none {
                it.type() == AclEntryType.ALLOW && it.principal() != owner && it.principal() !in administrators &&
                    it.permissions().any { permission -> permission in aclWrites }
            }
        }

    /** Permissions as persisted in local ownership records: POSIX keeps the established enum-name list, Windows the ACL text. */
    fun recordedPermissions(path: Path): String =
        if (posix) view<PosixFileAttributeView>(path).readAttributes().permissions().sorted().joinToString(",")
        else describe(path)

    /** Text for messages and identity comparison: the POSIX mode, or the ACL entries. */
    fun describe(path: Path): String =
        if (posix) PosixFilePermissions.toString(view<PosixFileAttributeView>(path).readAttributes().permissions())
        else view<AclFileAttributeView>(path).acl.joinToString(";") { "${it.type()}:${it.principal().name}:${it.permissions().sorted()}" }

    private inline fun <reified T : java.nio.file.attribute.FileAttributeView> view(path: Path): T =
        Files.getFileAttributeView(path, T::class.java, NOFOLLOW_LINKS)
            ?: throw UnsupportedOperationException("この環境ではファイルの権限を確認できません")

    private fun ownerAcl(directory: Boolean) = listOf(AclEntry.newBuilder()
        .setType(AclEntryType.ALLOW)
        .setPrincipal(owner)
        .setPermissions(AclEntryPermission.entries.toSet())
        .setFlags(if (directory) setOf(AclEntryFlag.FILE_INHERIT, AclEntryFlag.DIRECTORY_INHERIT) else emptySet())
        .build())

    /** Well-known SIDs have localized account names, so resolve them through the OS instead of by name. */
    private fun windowsPrincipal(sid: String): UserPrincipal? = try {
        val native = PointerByReference()
        check(WindowsAccounts.advapi.ConvertStringSidToSidW(WString(sid), native))
        try {
            val name = CharArray(256)
            val domain = CharArray(256)
            check(WindowsAccounts.advapi.LookupAccountSidW(null, native.value, name, IntByReference(name.size), domain,
                IntByReference(domain.size), IntByReference()))
            FileSystems.getDefault().userPrincipalLookupService.lookupPrincipalByName("${Native.toString(domain)}\\${Native.toString(name)}")
        } finally {
            WindowsAccounts.kernel.LocalFree(native.value)
        }
    } catch (_: Exception) {
        null
    } catch (_: LinkageError) {
        null
    }

    /**
     * Exclusive owner lock on one byte past any record data. Windows byte-range locks are mandatory, so a whole-file lock
     * would block other handles from reading the record; this byte still overlaps older whole-file locks.
     */
    fun tryOwnerLock(channel: FileChannel): java.nio.channels.FileLock? = channel.tryLock(Long.MAX_VALUE - 1, 1, false)

    /** Stable identity of the item at [path]; Windows reports no NIO file key, so use its volume and file index. */
    fun fileKey(path: Path): Any = if (posix)
        Files.readAttributes(path, java.nio.file.attribute.BasicFileAttributes::class.java, java.nio.file.LinkOption.NOFOLLOW_LINKS).fileKey()
            ?: error("ファイルの実体を確認できません")
    else WindowsSaveHandles.identity(path)
}

private object WindowsAccounts {
    init { StudioJna.ensure() }
    interface Advapi : Library {
        fun ConvertStringSidToSidW(sid: WString, out: PointerByReference): Boolean
        fun LookupAccountSidW(system: WString?, sid: Pointer, name: CharArray, nameLength: IntByReference, domain: CharArray,
            domainLength: IntByReference, use: IntByReference): Boolean
        fun LookupAccountNameW(system: WString?, account: WString, sid: ByteArray, sidLength: IntByReference, domain: CharArray,
            domainLength: IntByReference, use: IntByReference): Boolean
        fun ConvertSidToStringSidW(sid: ByteArray, out: PointerByReference): Boolean
        fun ConvertStringSecurityDescriptorToSecurityDescriptorW(descriptor: WString, revision: Int, out: PointerByReference,
            length: IntByReference?): Boolean
    }
    interface Kernel : Library {
        fun LocalFree(memory: Pointer): Pointer?
    }
    val advapi: Advapi by lazy { StudioJna.load("advapi32", Advapi::class.java) }
    val kernel: Kernel by lazy { StudioJna.load("kernel32", Kernel::class.java) }

}
