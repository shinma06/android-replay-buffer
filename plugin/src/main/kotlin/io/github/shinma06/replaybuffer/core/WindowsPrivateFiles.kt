package io.github.shinma06.replaybuffer.core

import com.sun.jna.Library
import com.sun.jna.Native
import com.sun.jna.Pointer
import com.sun.jna.WString
import com.sun.jna.ptr.IntByReference
import com.sun.jna.ptr.PointerByReference
import java.nio.channels.FileChannel
import java.nio.file.AccessDeniedException
import java.nio.file.FileAlreadyExistsException
import java.nio.file.FileSystems
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.OpenOption
import java.nio.file.Path
import java.nio.file.attribute.AclEntry
import java.nio.file.attribute.AclEntryFlag
import java.nio.file.attribute.AclEntryPermission
import java.nio.file.attribute.AclEntryType
import java.nio.file.attribute.AclFileAttributeView
import java.nio.file.attribute.FileAttribute
import java.nio.file.attribute.UserPrincipal

/**
 * Windows owner-only files: an ACL whose every entry allows the current user only, set from creation.
 * SYSTEM and Administrators play root's part: their access to a parent does not make it shared.
 */
internal object WindowsPrivateFiles : OwnerOnlyFiles {
    private val aclWrites = setOf(AclEntryPermission.WRITE_DATA, AclEntryPermission.APPEND_DATA, AclEntryPermission.DELETE_CHILD,
        AclEntryPermission.DELETE, AclEntryPermission.WRITE_ACL, AclEntryPermission.WRITE_OWNER)
    private val owner get() = PrivateFiles.owner
    private val administrators: Set<UserPrincipal> by lazy { setOfNotNull(windowsPrincipal("S-1-5-18"), windowsPrincipal("S-1-5-32-544")) }

    private fun attribute(directory: Boolean) = object : FileAttribute<List<AclEntry>> {
        override fun name() = "acl:acl"
        override fun value() = ownerAcl(directory)
    }

    override fun replace(source: Path, target: Path) = WindowsSaveHandles.replace(source, target)

    override fun createDirectory(path: Path): Path {
        Files.createDirectory(path, attribute(true))
        restrict(path, true)
        return path
    }

    override fun openNew(path: Path, options: Set<OpenOption>): FileChannel {
        val channel = try { FileChannel.open(path, options, attribute(false)) } catch (denied: AccessDeniedException) {
            // Windows reports CREATE_NEW over an existing directory as access denied; POSIX reports it as existing.
            if (Files.exists(path, NOFOLLOW_LINKS)) throw FileAlreadyExistsException(path.toString()).apply { initCause(denied) }
            throw denied
        }
        try {
            restrict(path, false)
        } catch (failure: Exception) {
            channel.close()
            throw failure
        }
        return channel
    }

    override fun restrict(path: Path, directory: Boolean) {
        attributeView<AclFileAttributeView>(path).let { if (it.owner != owner) it.owner = owner; it.acl = ownerAcl(directory) }
    }

    /** Runs [block] with a native owner-only security descriptor, so items are private from creation like mode 700/600. */
    fun <T> withOwnerOnlyDescriptor(directory: Boolean, block: (Pointer) -> T): T {
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

    override fun isPrivate(path: Path, directory: Boolean): Boolean = attributeView<AclFileAttributeView>(path).let { acl ->
        acl.owner == owner && acl.acl.all { it.type() == AclEntryType.ALLOW && it.principal() == owner }
    }

    override fun isOwnedWithoutSharedWrite(path: Path): Boolean = attributeView<AclFileAttributeView>(path).let { acl ->
        acl.owner == owner && acl.acl.none {
            it.type() == AclEntryType.ALLOW && it.principal() != owner && it.principal() !in administrators &&
                it.permissions().any { permission -> permission in aclWrites }
        }
    }

    override fun recordedPermissions(path: Path): String = describe(path)

    override fun describe(path: Path): String =
        attributeView<AclFileAttributeView>(path).acl.joinToString(";") { "${it.type()}:${it.principal().name}:${it.permissions().sorted()}" }

    /** Windows reports no NIO file key, so use its volume and file index. */
    override fun fileKey(path: Path): Any = WindowsSaveHandles.identity(path)

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
