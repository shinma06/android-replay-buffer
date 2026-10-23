package io.github.shinma06.replaybuffer.cli

import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import java.util.concurrent.CompletionStage
import java.util.concurrent.TimeUnit

/**
 * Waits on the CLI worker until a host-monotonic ns deadline; timeout does not cancel or retry the writer.
 * The admitted attempt owns its result even after OFF/UI changes; only COMPLETED yields a stdout path.
 */
internal fun cliSaveResponse(requestId: String?, result: CompletionStage<SaveSnapshot>, deadline: Long, warning: String): CliResponse {
    val saved = result.toCompletableFuture().get((deadline - System.nanoTime()).coerceAtLeast(1), TimeUnit.NANOSECONDS)
    check(requestId != null && saved.requestId == requestId) { "保存要求と結果が一致しません。" }
    return if (saved.phase == SavePhase.COMPLETED && saved.directory != null) {
        val missing = if (saved.missingKinds.isEmpty()) "" else "記録の不足・未確認: ${saved.missingKinds.joinToString()}。保存物のREADMEを確認してください。"
        CliResponse(0, saved.directory.toString(), listOf(warning, missing).filter(String::isNotEmpty).joinToString("\n"))
    } else {
        CliResponse(1, error = listOf(warning, saved.error ?: "保存は完了していません。GUIの状態を確認してください。",
            saved.retainedPath?.let { "保全した保存試行: $it" }.orEmpty(),
            "request: $requestId / attempt: ${saved.attempt}").filter(String::isNotEmpty).joinToString("\n"))
    }
}
