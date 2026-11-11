package io.github.shinma06.replaybuffer.settings;

import com.intellij.mock.MockApplication;
import com.intellij.mock.MockProject;
import com.intellij.openapi.application.ApplicationManager;
import com.intellij.openapi.client.ClientSession;
import com.intellij.openapi.client.ClientSessionsManager;
import com.intellij.openapi.extensions.ExtensionPoint;
import com.intellij.openapi.util.Disposer;
import com.intellij.ui.ExperimentalUI;
import com.intellij.ui.ExperimentalUIImpl;
import io.github.shinma06.replaybuffer.ide.ReplayProjectService;
import io.github.shinma06.replaybuffer.core.CaptureMode;
import io.github.shinma06.replaybuffer.core.VideoQuality;
import kotlinx.coroutines.CoroutineScope;
import org.junit.jupiter.api.Test;

import java.util.List;
import javax.swing.SwingUtilities;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ReplaySettingsLayoutTest {
    @Test
    void settingsDraftKeepsApplyResetSemanticsWithShortAndLongDestinations() throws Exception {
        var root = Disposer.newDisposable("settings layout test");
        var application = new MockApplication(root);
        ApplicationManager.setApplication(application, root);
        application.registerService(CoroutineScope.class, application.getCoroutineScope());
        application.registerService(ClientSessionsManager.class,
            new ClientSessionsManager<ClientSession>(application.getCoroutineScope()));
        application.getExtensionArea().registerExtensionPoint(
            "com.intellij.iconMapperSuppressor", Object.class.getName(), ExtensionPoint.Kind.INTERFACE, false);
        application.registerService(ExperimentalUI.class, new ExperimentalUIImpl());
        var project = new MockProject(null, root);
        var store = new ReplaySettingsStore();
        project.registerService(ReplaySettingsStore.class, store);
        assertNull(project.getService(ReplayProjectService.class));
        try {
            SwingUtilities.invokeAndWait(() -> {
                var longDestination = "/disposable/" + "long-directory/".repeat(12) + "save";
                for (var destination : List.of("", longDestination)) {
                    var saved = new ReplaySettingsState(false, 180, destination, AppSelectionMode.AUTOMATIC, "", CaptureMode.VIDEO_AND_LOGS, false, true, "", VideoQuality.STANDARD);
                    store.loadState(saved);
                    var configurable = new ReplaySettingsConfigurable(project);
                    try {
                        var form = SettingsFormTestSupport.createForm(configurable);
                        configurable.reset();
                        assertEquals(VideoQuality.STANDARD, form.getVideoQuality());
                        assertTrue(form.getControls().getVideoOptionsEnabled());
                        form.setVideoQuality(VideoQuality.LIGHTWEIGHT);
                        assertTrue(configurable.isModified());
                        form.setCaptureMode(CaptureMode.LOG_ONLY);
                        assertFalse(form.getControls().getVideoOptionsEnabled(), "log-only disables video options");
                        assertEquals(VideoQuality.LIGHTWEIGHT, form.getVideoQuality());
                        configurable.reset();
                        assertEquals(VideoQuality.STANDARD, form.getVideoQuality());
                        assertTrue(form.getControls().getVideoOptionsEnabled());
                        assertFalse(form.getControls().getManualPackageEnabled(), "automatic selection keeps the package field read-only");
                        assertNull(form.getControls().getRecordingNotice(), "no capture lock note while OFF");
                        assertTrue(ReplaySettingsConfigurableKt.APPLY_GUIDANCE.contains("キャンセル"));
                        assertTrue(ReplaySettingsConfigurableKt.APPLY_GUIDANCE.contains("ON/OFF"));
                        assertTrue(ReplaySettingsConfigurableKt.AUTO_SAVE_TOOLTIP.contains("この設定をOFFで適用してから、ONで再び適用"));
                        assertFalse(ReplaySettingsConfigurableKt.AUTO_SAVE_TOOLTIP.contains("取得OFF→ON"));
                        assertEquals(destination, form.getDestination().getText().toString());
                        assertFalse(configurable.isModified());
                        assertEquals(saved, store.getState(), "opening settings must not apply settings");
                        assertTrue(form.getScreenshotUsesLogcat());
                        assertFalse(form.getControls().getScreenshotDirectoryEnabled());
                        form.setScreenshotUsesLogcat(false);
                        assertTrue(form.getControls().getScreenshotDirectoryEnabled());
                        assertTrue(configurable.isModified());
                        assertTrue(store.settings().getScreenshotUsesLogcatDestination(), "draft must not change effective destination");
                        SettingsFormTestSupport.setText(form.getScreenshotDirectory(), "/disposable/shots");
                        assertEquals("", store.settings().getScreenshotDirectory());
                        configurable.reset();
                        assertTrue(form.getScreenshotUsesLogcat());
                        assertFalse(form.getControls().getScreenshotDirectoryEnabled());
                        assertEquals("", form.getScreenshotDirectory().getText().toString());
                        assertFalse(configurable.isModified(), "Cancel/reset discards screenshot destination edits");
                        assertFalse(form.getAutoSave());
                        form.setAutoSave(true);
                        assertTrue(configurable.isModified());
                        assertFalse(store.settings().getAutoSaveOnCrash(), "draft must not change effective preference");
                        configurable.reset();
                        assertFalse(form.getAutoSave());
                        assertFalse(configurable.isModified());
                    } catch (ReflectiveOperationException failure) {
                        throw new AssertionError(failure);
                    } finally {
                        configurable.disposeUIResources();
                    }
                }
            });
        } finally {
            Disposer.dispose(root);
        }
    }
}
