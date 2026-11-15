package io.github.shinma06.replaybuffer.ui

import com.intellij.testFramework.LightVirtualFile
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.io.ByteArraySequence
import java.lang.reflect.Proxy
import javax.xml.parsers.DocumentBuilderFactory
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertSame
import kotlin.test.assertTrue
import org.w3c.dom.Element

class ReplayVideoFileTypeTest {
    private class Node(name: String, private val directory: Boolean = false) : LightVirtualFile(name) {
        var owner: Node? = null
        val nodes = mutableListOf<Node>()
        override fun isDirectory() = directory
        override fun getParent() = owner
        override fun getChildren() = nodes.toTypedArray()
    }
    private fun file(name: String) = Node(name)
    private fun dir(name: String, vararg children: Node) = Node(name, true).apply {
        children.forEach { it.owner = this; nodes.add(it) }
    }
    private val detector = ReplayVideoFileTypeDetector()
    private val empty = ByteArraySequence(byteArrayOf())

    @Test
    fun savedVideoIsRecognizedWithoutClaimingOtherMp4Files() {
        val video = file("video.mp4")
        val other = file("other.mp4")
        dir("replay", video, other, file("replay.json"), file("video-frames.jsonl"))
        val project = Proxy.newProxyInstance(javaClass.classLoader, arrayOf(Project::class.java)) { _, _, _ ->
            error("file recognition does not need project services")
        } as Project
        assertSame(ReplayVideoFileType, detector.detect(video, empty, null))
        assertTrue(ReplayPlayerEditorProvider().accept(project, video))
        assertNull(detector.detect(other, empty, null))
        assertFalse(ReplayPlayerEditorProvider().accept(project, other))
        assertTrue(ReplayVideoFileType.isBinary)
    }

    @Test
    fun unrelatedVideosAndDirectoryMarkersAreNotReplayFiles() {
        val video = file("video.mp4")
        dir("unrelated", video)
        assertNull(detector.detect(video, empty, null))
        dir("incomplete", video, file("replay.json"))
        assertNull(detector.detect(video, empty, null))
        dir("directories", video, dir("replay.json"), file("video-frames.jsonl"))
        assertNull(detector.detect(video, empty, null))
        val directory = dir("video.mp4")
        dir("replay", directory, file("replay.json"), file("video-frames.jsonl"))
        assertNull(detector.detect(directory, empty, null))
    }

    @Test
    fun descriptorRegistersTheDetectorWithoutAGlobalMp4Association() {
        val document = javaClass.getResourceAsStream("/META-INF/plugin.xml")!!.use {
            DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(it)
        }
        val types = document.getElementsByTagName("fileType")
        val type = (0 until types.length).map { types.item(it) as Element }
            .single { it.getAttribute("name") == ReplayVideoFileType.name }
        assertEquals(ReplayVideoFileType::class.java.name, type.getAttribute("implementationClass"))
        assertEquals("INSTANCE", type.getAttribute("fieldName"))
        assertEquals("", type.getAttribute("extensions"))
        assertEquals("", type.getAttribute("fileNames"))
        val detectors = document.getElementsByTagName("fileTypeDetector")
        assertTrue((0 until detectors.length).any {
            (detectors.item(it) as Element).getAttribute("implementation") == ReplayVideoFileTypeDetector::class.java.name
        })
    }
}
