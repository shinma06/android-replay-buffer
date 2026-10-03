package io.github.shinma06.replaybuffer.core

import com.sun.jna.Library
import com.sun.jna.Native
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path

/** Use the IDE's existing JNA, not another runtime dependency or a bundled native binary. */
internal interface MacExclusiveRename : Library {
    fun renamex_np(source: String, destination: String, flags: Int): Int
}

private val exclusiveRename by lazy {
    Native.load("c", MacExclusiveRename::class.java, mapOf(Library.OPTION_STRING_ENCODING to "UTF-8"))
}

/** One syscall atomically publishes the whole folder and rejects even an existing empty directory. */
internal fun publishCapture(partial: Path, complete: Path) {
    check(System.getProperty("os.name") == "Mac OS X") { "この環境は原子・非上書き保存に対応していません" }
    require(partial.parent == complete.parent && partial.isAbsolute && complete.isAbsolute)
    require(Files.isDirectory(partial, NOFOLLOW_LINKS) && partial.parent == partial.parent.toRealPath())
    val result = try { exclusiveRename.renamex_np(partial.toString(), complete.toString(), 0x4) } catch (_: LinkageError) {
        error("原子・非上書き保存を利用できません")
    }
    check(result == 0) { "完成先の公開に失敗しました（既存出力・対応filesystem・アクセス権を確認してください）" }
    // No Java rename fallback: unsupported filesystems must leave the pinned request retryable.
}
