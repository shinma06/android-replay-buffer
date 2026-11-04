package io.github.shinma06.replaybuffer.ide

import java.io.IOException
import java.nio.file.FileAlreadyExistsException
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.attribute.PosixFileAttributeView
import java.nio.file.attribute.PosixFileAttributes
import java.nio.file.attribute.PosixFilePermission
import java.nio.file.attribute.PosixFilePermissions
import java.nio.file.attribute.UserPrincipal
import java.security.MessageDigest

internal class CleanupDirectoryFailure(message: String, cause: Throwable? = null) : IllegalArgumentException(message, cause)

private val privateMode = PosixFilePermissions.fromString("rwx------")
private val sharedWrite = setOf(PosixFilePermission.GROUP_WRITE, PosixFilePermission.OTHERS_WRITE)

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
    if (configAttributes.owner() != owner || configAttributes.permissions().any { it in sharedWrite }) {
        throw CleanupDirectoryFailure("IDE設定領域（.）は本人所有でgroup/otherの書込み不可である必要があります。" +
            "現在の権限: ${PosixFilePermissions.toString(configAttributes.permissions())}。")
    }
    val projectId = java.util.HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(project.toString().toByteArray(Charsets.UTF_8)))
    var directory = config
    for (name in listOf("android-replay-buffer", "cleanup", projectId)) {
        directory = directory.resolve(name)
        preparePrivateDirectory(directory, config.relativize(directory), owner)
    }
    return directory
}

internal fun preparePrivateDirectory(directory: Path, relative: Path, owner: UserPrincipal) {
    var mode = "不明"
    fun failure(reason: String, cause: Throwable? = null) =
        CleanupDirectoryFailure("終了情報の保存先「$relative」は本人所有のディレクトリ・権限700が必要です。" +
            "現在の権限: $mode。$reason", cause)

    try {
        try {
            Files.createDirectory(directory, PosixFilePermissions.asFileAttribute(privateMode))
        } catch (_: FileAlreadyExistsException) {
            // Validate each parent before using it to reach the next component.
        }
        val view = Files.getFileAttributeView(directory, PosixFileAttributeView::class.java, NOFOLLOW_LINKS)
            ?: throw failure("所有者と権限を確認できません。")
        val before = view.readAttributes()
        mode = PosixFilePermissions.toString(before.permissions())
        if (before.isSymbolicLink) throw failure("シンボリックリンクは追跡しません。")
        if (!before.isDirectory) throw failure("ディレクトリではない項目は変更しません。")
        if (before.owner() != owner) throw failure("所有者が異なるため変更しません。")
        if (before.permissions() == privateMode) return
        if (!before.permissions().containsAll(privateMode) || before.permissions().any { it in sharedWrite }) {
            throw failure("本人の権限不足、または他の利用者に書込み可能な項目は変更しません。")
        }
        // An import can add read/search bits. Never follow a link or grant a missing owner permission.
        view.setPermissions(privateMode)
        val after = view.readAttributes()
        mode = PosixFilePermissions.toString(after.permissions())
        if (!after.isDirectory || after.owner() != owner || after.fileKey() != before.fileKey() || after.permissions() != privateMode) {
            throw failure("同じディレクトリの権限を確認できません。")
        }
    } catch (cause: IOException) {
        throw failure("設定領域へのアクセスを確認してください。", cause)
    } catch (cause: UnsupportedOperationException) {
        throw failure("この環境では安全に権限を変更できません。", cause)
    } catch (cause: SecurityException) {
        throw failure("設定領域へのアクセスを確認してください。", cause)
    }
}
