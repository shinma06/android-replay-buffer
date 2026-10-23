package io.github.shinma06.replaybuffer.cli

import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import java.util.concurrent.CompletionStage
import java.util.concurrent.TimeUnit

/** A result belongs to the admitted writer attempt, even if OFF or a retry changes the UI snapshot. */
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
