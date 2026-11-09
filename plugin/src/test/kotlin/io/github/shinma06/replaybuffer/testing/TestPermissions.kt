package io.github.shinma06.replaybuffer.testing

import java.nio.file.FileSystems
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.attribute.AclEntry
import java.nio.file.attribute.AclEntryFlag
import java.nio.file.attribute.AclEntryPermission
import java.nio.file.attribute.AclEntryPermission.*
import java.nio.file.attribute.AclEntryType
import java.nio.file.attribute.AclFileAttributeView
import java.nio.file.attribute.PosixFilePermissions
import java.nio.file.attribute.UserPrincipal

/**
 * POSIX modes for tests on every OS. On Windows group maps to BUILTIN\Users and other to Everyone; an owner without
 * write on a directory also denies deleting its current children, because Windows lets a child's own ACL allow deletion.
 */
object TestPermissions {
    private val windows = System.getProperty("os.name").startsWith("Windows")
    private val lookup by lazy { FileSystems.getDefault().userPrincipalLookupService }
    private val owner: UserPrincipal by lazy { lookup.lookupPrincipalByName(System.getProperty("user.name")) }
    private val users: UserPrincipal by lazy { lookup.lookupPrincipalByName("BUILTIN\\Users") }
    private val everyone: UserPrincipal by lazy { lookup.lookupPrincipalByName("Everyone") }
    private val writes = setOf(WRITE_DATA, APPEND_DATA, DELETE_CHILD)
    private val deniedChildren = mutableMapOf<Path, Map<Path, List<AclEntry>>>()

    @JvmStatic fun mode(mode: String): String = mode

    @JvmStatic fun set(path: Path, mode: String) {
        if (!windows) { Files.setPosixFilePermissions(path, PosixFilePermissions.fromString(mode)); return }
        val directory = Files.isDirectory(path, NOFOLLOW_LINKS)
        deniedChildren.remove(path)?.forEach { (child, acl) -> view(child).acl = acl }
        val entries = mutableListOf<AclEntry>()
        if (directory && mode[1] != 'w') {
            entries += entry(AclEntryType.DENY, owner, writes, false)
            val children = Files.list(path).use { stream -> stream.toList() }.associateWith { view(it).acl }
            children.forEach { (child, acl) -> view(child).acl = listOf(entry(AclEntryType.DENY, owner, setOf(DELETE), false)) + acl }
            deniedChildren[path] = children
        }
        entries += entry(AclEntryType.ALLOW, owner, bits(mode.substring(0, 3)) + setOf(READ_ACL, WRITE_ACL, READ_ATTRIBUTES, SYNCHRONIZE), directory)
        if (mode.substring(3, 6) != "---") entries += entry(AclEntryType.ALLOW, users, bits(mode.substring(3, 6)), directory)
        if (mode.substring(6, 9) != "---") entries += entry(AclEntryType.ALLOW, everyone, bits(mode.substring(6, 9)), directory)
        view(path).acl = entries
    }

    @JvmStatic fun get(path: Path): String {
        if (!windows) return PosixFilePermissions.toString(Files.getPosixFilePermissions(path, NOFOLLOW_LINKS))
        val directory = Files.isDirectory(path, NOFOLLOW_LINKS)
        val acl = view(path).acl
        fun text(principal: (UserPrincipal) -> Boolean, denyWrite: Boolean): String {
            val allowed = acl.filter { it.type() == AclEntryType.ALLOW && principal(it.principal()) }.flatMap { it.permissions() }.toSet()
            return (if (READ_DATA in allowed) "r" else "-") + (if (WRITE_DATA in allowed && !denyWrite) "w" else "-") +
                (if (directory && EXECUTE in allowed) "x" else "-")
        }
        val ownerDenied = acl.any { it.type() == AclEntryType.DENY && it.principal() == owner && WRITE_DATA in it.permissions() }
        return text({ it == owner }, ownerDenied) + text({ it != owner && it != everyone }, false) + text({ it == everyone }, false)
    }

    private fun bits(mode: String): Set<AclEntryPermission> = buildSet {
        if (mode[0] == 'r') addAll(listOf(READ_DATA, READ_NAMED_ATTRS, READ_ATTRIBUTES, READ_ACL, SYNCHRONIZE))
        if (mode[1] == 'w') addAll(listOf(WRITE_DATA, APPEND_DATA, WRITE_NAMED_ATTRS, WRITE_ATTRIBUTES, DELETE, DELETE_CHILD))
        if (mode[2] == 'x') add(EXECUTE)
    }

    private fun entry(type: AclEntryType, principal: UserPrincipal, permissions: Set<AclEntryPermission>, inherit: Boolean) =
        AclEntry.newBuilder().setType(type).setPrincipal(principal).setPermissions(permissions)
            .setFlags(if (inherit) setOf(AclEntryFlag.FILE_INHERIT, AclEntryFlag.DIRECTORY_INHERIT) else emptySet()).build()

    private fun view(path: Path) = Files.getFileAttributeView(path, AclFileAttributeView::class.java, NOFOLLOW_LINKS)
}
