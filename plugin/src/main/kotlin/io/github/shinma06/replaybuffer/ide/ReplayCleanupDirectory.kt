package io.github.shinma06.replaybuffer.ide

import java.nio.file.FileAlreadyExistsException
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.attribute.PosixFileAttributeView
import java.nio.file.attribute.PosixFileAttributes
import java.nio.file.attribute.PosixFilePermission
import java.nio.file.attribute.PosixFilePermissions
import java.security.MessageDigest

/** Filesystem work only, called on IO. No project/config path is stored in cleanup records. */
internal fun prepareCleanupDirectory(configDirectory: Path, projectBasePath: String?): Path {
    require(!projectBasePath.isNullOrBlank()) { "projectの保存場所を確認できません。" }
    val projectPath = Path.of(projectBasePath)
    require(projectPath.isAbsolute && configDirectory.isAbsolute) { "projectとIDE設定領域の絶対パスを確認してください。" }
    val project = projectPath.toRealPath()
    require(Files.isDirectory(project)) { "projectの保存場所を確認できません。" }
    val config = configDirectory.toRealPath()
    require(Files.getFileAttributeView(config, PosixFileAttributeView::class.java) != null) {
        "この環境では終了情報の保存先を安全に保護できません。"
    }
    val owner = config.fileSystem.userPrincipalLookupService.lookupPrincipalByName(System.getProperty("user.name"))
    val configAttributes = Files.readAttributes(config, PosixFileAttributes::class.java, NOFOLLOW_LINKS)
    require(configAttributes.owner() == owner && configAttributes.permissions().none {
        it == PosixFilePermission.GROUP_WRITE || it == PosixFilePermission.OTHERS_WRITE
    }) { "IDE設定領域の所有者とアクセス権を確認してください。" }
    val projectId = MessageDigest.getInstance("SHA-256").digest(project.toString().toByteArray(Charsets.UTF_8))
        .joinToString("") { "%02x".format(it) }
    val permissions = PosixFilePermissions.fromString("rwx------")
    var directory = config
    for (name in listOf("android-replay-buffer", "cleanup", projectId)) {
        directory = directory.resolve(name)
        try {
            Files.createDirectory(directory, PosixFilePermissions.asFileAttribute(permissions))
        } catch (_: FileAlreadyExistsException) {
            // Inspect the exact existing entry, never follow or replace it.
        }
        val attributes = Files.readAttributes(directory, PosixFileAttributes::class.java, NOFOLLOW_LINKS)
        require(attributes.isDirectory && attributes.owner() == owner && attributes.permissions() == permissions) {
            "終了情報の保存先の所有者とアクセス権を確認してください。"
        }
    }
    return directory
}
