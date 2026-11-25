package io.github.shinma06.arb.ide

import io.github.shinma06.arb.ipc.ArbdRpcClient
import io.github.shinma06.arb.model.ArbdRpcException
import io.github.shinma06.arb.model.MediaOpenViewportResponse
import io.github.shinma06.arb.shm.ShmFrame
import io.github.shinma06.arb.shm.ShmFrameRingReader
import java.io.Closeable
import java.io.IOException
import java.nio.file.Path

/** One connection generation owns one viewport and reader. Calls are serialized per editor. */
internal class StudioViewportSession(
    private val connect: () -> ArbdRpcClient,
    private val disposed: () -> Boolean,
) : Closeable {
    data class ReadResult<T>(val response: T, val frame: ShmFrame?, val replacementId: String?)

    private data class Generation(
        val rpc: ArbdRpcClient,
        val viewport: MediaOpenViewportResponse,
        val reader: ShmFrameRingReader,
    )

    private var current: Generation? = null
    private var sourceBundleId: String? = null

    @Synchronized
    fun open(bundleId: String): MediaOpenViewportResponse {
        check(current == null && !disposed())
        sourceBundleId = bundleId
        val rpc = connect()
        try {
            val viewport = rpc.mediaOpenViewport(bundleId)
            try {
                check(!disposed())
                val reader = ShmFrameRingReader.open(Path.of(viewport.shmPath))
                current = Generation(rpc, viewport, reader)
                return viewport
            } catch (e: Exception) {
                runCatching { rpc.mediaCloseViewport(viewport.viewportId) }
                throw e
            }
        } catch (e: Exception) {
            rpc.close()
            throw e
        }
    }

    @Synchronized
    fun <T> request(onRecovery: () -> Unit = {}, call: (ArbdRpcClient, String) -> T): ReadResult<T> {
        check(!disposed())
        val reopened = current == null
        if (reopened) {
            onRecovery()
            open(checkNotNull(sourceBundleId))
        }
        val generation = checkNotNull(current)
        val result = try {
            call(generation.rpc, generation.viewport.viewportId)
        } catch (e: Exception) {
            if (e !is IOException && (e !is ArbdRpcException || e.code != VIEWPORT_NOT_FOUND)) throw e
            // Disconnecting the owning socket also reclaims a viewport whose reply was lost.
            onRecovery()
            generation.rpc.close()
            generation.reader.close()
            current = null
            try {
                open(generation.viewport.bundleId)
                val fresh = checkNotNull(current)
                return ReadResult(call(fresh.rpc, fresh.viewport.viewportId), fresh.reader.readLatest(), fresh.viewport.viewportId)
            } catch (recovery: Exception) {
                throw IllegalStateException("映像接続を復元できませんでした（${recovery.message}）", recovery)
            }
        }
        return ReadResult(result, generation.reader.readLatest(), generation.viewport.viewportId.takeIf { reopened })
    }

    @Synchronized
    override fun close() {
        val generation = current ?: return
        current = null
        runCatching { generation.rpc.mediaCloseViewport(generation.viewport.viewportId) }
        generation.rpc.close()
        generation.reader.close()
    }

    private companion object {
        const val VIEWPORT_NOT_FOUND = -32001
    }
}
