package io.github.shinma06.replaybuffer.ide

import java.io.IOException
import java.nio.file.FileAlreadyExistsException
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import io.github.shinma06.replaybuffer.core.PrivateFiles
import java.nio.file.attribute.BasicFileAttributes
import java.nio.file.attribute.UserPrincipal
import java.security.MessageDigest

internal class CleanupDirectoryFailure(message: String, cause: Throwable? = null) : IllegalArgumentException(message, cause)

/** Filesystem work only, called on IO. No project/config path is stored in cleanup records. */
internal fun prepareCleanupDirectory(configDirectory: Path, projectBasePath: String?): Path {
    require(!projectBasePath.isNullOrBlank()) { "projectの保存場所を確認できません。" }
    val projectPath = Path.of(projectBasePath)
    require(projectPath.isAbsolute && configDirectory.isAbsolute) { "projectとIDE設定領域の絶対パスを確認してください。" }
    val project = projectPath.toRealPath()
    require(Files.isDirectory(project)) { "projectの保存場所を確認できません。" }
    val config = configDirectory.toRealPath()
    val owner = PrivateFiles.owner
    val shared = try { !PrivateFiles.isOwnedWithoutSharedWrite(config) } catch (cause: UnsupportedOperationException) {
        throw IllegalArgumentException("この環境では終了情報の保存先を安全に保護できません。", cause)
    }
    if (shared) {
        throw CleanupDirectoryFailure("IDE設定領域（.）は本人所有で他の利用者の書込み不可である必要があります。" +
            "現在の権限: ${PrivateFiles.describe(config)}。")
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
            PrivateFiles.createDirectory(directory)
        } catch (_: FileAlreadyExistsException) {
            // Validate each parent before using it to reach the next component.
        }
        val before = Files.readAttributes(directory, BasicFileAttributes::class.java, NOFOLLOW_LINKS)
        val beforeKey = PrivateFiles.fileKey(directory)
        mode = PrivateFiles.describe(directory)
        if (before.isSymbolicLink) throw failure("シンボリックリンクは追跡しません。")
        if (!before.isDirectory) throw failure("ディレクトリではない項目は変更しません。")
        if (Files.getOwner(directory, NOFOLLOW_LINKS) != owner) throw failure("所有者が異なるため変更しません。")
        if (PrivateFiles.isPrivate(directory, true)) return
        if (!Files.isReadable(directory) || !Files.isWritable(directory) || !Files.isExecutable(directory) ||
            !PrivateFiles.isOwnedWithoutSharedWrite(directory)) {
            throw failure("本人の権限不足、または他の利用者に書込み可能な項目は変更しません。")
        }
        // An import can add read/search access for others. Never follow a link or grant a missing owner permission.
        PrivateFiles.restrict(directory, true)
        val after = Files.readAttributes(directory, BasicFileAttributes::class.java, NOFOLLOW_LINKS)
        mode = PrivateFiles.describe(directory)
        if (!after.isDirectory || Files.getOwner(directory, NOFOLLOW_LINKS) != owner || PrivateFiles.fileKey(directory) != beforeKey ||
            !PrivateFiles.isPrivate(directory, true)) {
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
