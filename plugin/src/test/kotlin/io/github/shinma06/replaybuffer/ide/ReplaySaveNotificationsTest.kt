package io.github.shinma06.replaybuffer.ide

import com.intellij.mock.MockApplication
import com.intellij.mock.MockProject
import com.intellij.notification.Notification
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationDisplayType
import com.intellij.notification.NotificationGroup
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.notification.NotificationsConfiguration
import com.intellij.notification.impl.NotificationsConfigurationImpl
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.util.Disposer
import com.intellij.ui.SystemNotifications
import io.github.shinma06.replaybuffer.core.SaveContent
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.menubar.ReplayMenuBarStore
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import org.junit.jupiter.api.Test
import java.nio.file.Path
import java.util.concurrent.TimeUnit
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertSame
import kotlin.test.assertTrue

class ReplaySaveNotificationsTest {
    @Test
    fun `only failures and completed crash saves use one background popup channel`() = Fixture().use { fixture ->
        val project = fixture.project("First")
        for (content in SaveContent.entries) for (automatic in listOf(false, true)) for (phase in SavePhase.entries) {
            val save = SaveSnapshot(phase, "request", directory = Path.of("completed"),
                autoEventId = if (automatic) "crash" else null, content = content)
            val notification = fixture.notification()
            val previous = fixture.deliveries.size
            val actions = notification.actions.toList()
            notifyBackgroundSave(save, notification, project)
            val expected = phase == SavePhase.FAILED || phase == SavePhase.COMPLETED && automatic
            assertEquals(previous + if (expected) 1 else 0, fixture.deliveries.size, "$phase / $automatic / $content")
            assertEquals(expected, notification.isShowingPopupSuppressed)
            assertEquals(actions, notification.actions)
            assertFalse(notification.isExpired)
            if (expected) {
                val delivery = fixture.deliveries.last()
                assertEquals(notification.id, delivery.name)
                assertEquals("First — " + if (phase == SavePhase.FAILED) "保存に失敗しました" else "クラッシュ時の記録を自動保存しました", delivery.title)
                assertFalse(delivery.text.contains("<"))
            }
        }
        val incomplete = fixture.notification()
        val before = fixture.deliveries.size
        notifyBackgroundSave(SaveSnapshot(SavePhase.COMPLETED, "incomplete", autoEventId = "crash"), incomplete, project)
        assertEquals(before, fixture.deliveries.size)
        assertFalse(incomplete.isShowingPopupSuppressed)
    }

    @Test
    fun `foreground and each disabled setting preserve the original IDE popup`() {
        for (disabled in listOf("foreground", "menu", "system", "balloons", "group", "headless", "unit-test")) Fixture().use { fixture ->
            when (disabled) {
                "foreground" -> fixture.active = true
                "menu" -> fixture.menu.showInMenuBar = false
                "system" -> fixture.configuration.SYSTEM_NOTIFICATIONS = false
                "balloons" -> fixture.configuration.SHOW_BALLOONS = false
                "group" -> fixture.configuration.setDisplayType(GROUP, NotificationDisplayType.NONE)
                "headless" -> fixture.headless = true
                "unit-test" -> fixture.unitTest = true
            }
            for (save in listOf(SaveSnapshot(SavePhase.FAILED, "failed"),
                SaveSnapshot(SavePhase.COMPLETED, "automatic", directory = Path.of("completed"), autoEventId = "crash"))) {
                val notification = fixture.notification()
                notifyBackgroundSave(save, notification, fixture.project("First"))
                assertTrue(fixture.deliveries.isEmpty(), disabled)
                assertFalse(notification.isShowingPopupSuppressed, disabled)
                assertFalse(notification.isExpired, disabled)
                assertEquals(1, notification.actions.size, disabled)
            }
        }
    }

    @Test
    fun `history opt out keeps details and actions in the existing IDE popup`() = Fixture().use { fixture ->
        fixture.configuration.changeSettings(GROUP, NotificationDisplayType.BALLOON, false, false)
        assertFalse(NotificationsConfigurationImpl.getSettings(GROUP).isShouldLog)
        for (save in listOf(SaveSnapshot(SavePhase.FAILED, "failed"),
            SaveSnapshot(SavePhase.COMPLETED, "automatic", directory = Path.of("completed"), autoEventId = "crash"))) {
            val notification = fixture.notification()
            val action = notification.actions.single()
            notifyBackgroundSave(save, notification, fixture.project("First"))
            assertTrue(fixture.deliveries.isEmpty())
            assertFalse(notification.isShowingPopupSuppressed)
            assertFalse(notification.isExpired)
            assertSame(action, notification.actions.single())
        }
    }

    @Test
    fun `SDK failure leaves the IDE notification and its action intact`() = Fixture().use { fixture ->
        fixture.failDelivery = true
        val notification = fixture.notification()
        val action = notification.actions.single()
        notifyBackgroundSave(SaveSnapshot(SavePhase.FAILED, "failed"), notification, fixture.project("First"))
        assertTrue(fixture.deliveries.isEmpty())
        assertFalse(notification.isShowingPopupSuppressed)
        assertFalse(notification.isExpired)
        assertSame(action, notification.actions.single())
    }

    @Test
    fun `project notifications have distinct native identities and closed projects cannot notify`() = Fixture().use { fixture ->
        val first = fixture.project("First")
        val second = fixture.project("Second")
        val save = SaveSnapshot(SavePhase.FAILED, "same-request", attempt = 1)
        notifyBackgroundSave(save, fixture.notification(), first)
        notifyBackgroundSave(save, fixture.notification(), second)
        assertEquals(listOf("First — 保存に失敗しました", "Second — 保存に失敗しました"), fixture.deliveries.map { it.title })
        assertNotEquals(fixture.deliveries[0].name, fixture.deliveries[1].name)
        Disposer.dispose(first)
        val late = fixture.notification()
        notifyBackgroundSave(save.copy(attempt = 2), late, first)
        assertEquals(2, fixture.deliveries.size)
        assertFalse(late.isShowingPopupSuppressed)
    }

    @Test
    fun `shared capture terminal transitions notify once per attempt independently for each project`() = Fixture().use { fixture ->
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val notifications = mutableListOf<Notification>()
        val environment = AndroidReplayEnvironment(null, "SDK未設定", ApplicationSelection(null, null, "未選択"))
        try {
            for (label in listOf("First", "Second")) {
                val project = fixture.project(label)
                val capture = ReplayProjectCapture(scope, ReplaySettingsStore(), {}, { save ->
                    val notification = fixture.notification()
                    notifyBackgroundSave(save, notification, project)
                    notifications += notification
                })
                try {
                    assertTrue(capture.applySettings(ReplaySettings(), environment, 0).get(15, TimeUnit.SECONDS).accepted)
                    var next = capture.view.snapshot!!.let { it.copy(revision = it.revision + 100) }
                    fun publish(save: SaveSnapshot) {
                        next = next.copy(revision = next.revision + 1, save = save)
                        capture.acceptSnapshot(next)
                    }
                    val failed = SaveSnapshot(SavePhase.FAILED, "same-request", attempt = 1)
                    publish(failed)
                    repeat(3) { publish(failed) }
                    capture.acceptSnapshot(next.copy(revision = next.revision - 1))
                    publish(failed.copy(phase = SavePhase.WRITING, attempt = 2))
                    publish(failed.copy(attempt = 2))
                    val manual = SaveSnapshot(SavePhase.COMPLETED, "manual", directory = Path.of("completed"), attempt = 3)
                    publish(manual)
                    val automatic = manual.copy(requestId = "automatic", attempt = 4, autoEventId = "crash")
                    publish(automatic)
                    repeat(3) { publish(automatic) }
                    capture.close()
                    capture.termination.get(15, TimeUnit.SECONDS)
                    publish(failed.copy(requestId = "late", attempt = 5))
                } finally {
                    capture.close()
                    capture.termination.get(15, TimeUnit.SECONDS)
                }
            }
            assertEquals(8, notifications.size)
            assertEquals(listOf(true, true, false, true, true, true, false, true), notifications.map { it.isShowingPopupSuppressed })
            assertEquals(6, fixture.deliveries.size)
            assertEquals(6, fixture.deliveries.map { it.name }.distinct().size)
            assertTrue(notifications.all { it.actions.size == 1 && !it.isExpired })
        } finally {
            scope.cancel()
        }
    }

    private data class Delivery(val name: String, val title: String, val text: String)

    private class Fixture : AutoCloseable {
        private val lifetime = Disposer.newDisposable("save notification test")
        var active = false
        var headless = false
        var unitTest = false
        var failDelivery = false
        val deliveries = mutableListOf<Delivery>()
        val menu = ReplayMenuBarStore().apply { showInMenuBar = true }
        val configuration = NotificationsConfigurationImpl()
        private val application = object : MockApplication(lifetime) {
            override fun isActive() = active
            override fun isHeadlessEnvironment() = headless
            override fun isUnitTestMode() = unitTest
        }

        init {
            ApplicationManager.setApplication(application, lifetime)
            application.registerService(ReplayMenuBarStore::class.java, menu)
            application.registerService(NotificationsConfiguration::class.java, configuration)
            application.registerService(NotificationGroupManager::class.java, object : NotificationGroupManager {
                override fun getNotificationGroup(id: String): NotificationGroup = NotificationGroup.create(
                    id, NotificationDisplayType.BALLOON, true, null, null, PluginId.getId("com.intellij"))
                override fun isGroupRegistered(id: String) = true
                override fun getRegisteredNotificationGroups() = emptyList<NotificationGroup>()
                override fun isRegisteredNotificationId(id: String) = false
            })
            application.registerService(SystemNotifications::class.java, object : SystemNotifications() {
                override fun notify(notificationName: String, title: String, text: String) {
                    if (failDelivery) throw IllegalStateException("synthetic notification failure")
                    deliveries += Delivery(notificationName, title, text)
                }
            })
        }

        fun project(label: String) = object : MockProject(null, lifetime) { override fun getName() = label }
        fun notification() = Notification(GROUP, "保存結果", "<b>既存の詳細</b>", NotificationType.INFORMATION)
            .addAction(NotificationAction.createSimple("既存のアクション") {})
        override fun close() = Disposer.dispose(lifetime)
    }

    companion object {
        private const val GROUP = "Android Replay Buffer"
    }
}
