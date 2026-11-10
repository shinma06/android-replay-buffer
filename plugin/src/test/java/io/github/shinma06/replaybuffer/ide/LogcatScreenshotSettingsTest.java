package io.github.shinma06.replaybuffer.ide;

import com.android.tools.idea.ui.save.SaveConfigurationResolver;
import com.android.tools.idea.ui.screenshot.DeviceScreenshotSettings;
import com.intellij.mock.MockApplication;
import com.intellij.mock.MockProject;
import com.intellij.openapi.application.ApplicationManager;
import com.intellij.openapi.util.Disposer;
import com.intellij.util.SystemProperties;
import java.nio.file.Path;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import static org.junit.jupiter.api.Assertions.*;

class LogcatScreenshotSettingsTest {
    @TempDir Path directory;

    @Test
    void usesTheNativeLiveSettingsAndNativeLocationRules() {
        var lifetime = Disposer.newDisposable("Logcat screenshot settings test");
        var app = new MockApplication(lifetime);
        ApplicationManager.setApplication(app, lifetime);
        var project = new MockProject(null, lifetime) {
            @Override public String getName() { return "screenshot-fixture"; }
        };
        var nativeSettings = new DeviceScreenshotSettings();
        app.registerService(DeviceScreenshotSettings.class, nativeSettings);
        project.registerService(SaveConfigurationResolver.class, new SaveConfigurationResolver(project));
        try {
            assertEquals(SaveConfigurationResolver.DEFAULT_SAVE_LOCATION, LogcatScreenshotSettings.saveLocation());
            assertEquals("$DESKTOP_DIR$", LogcatScreenshotSettings.saveLocation(), "absent native state uses the IDE default");
            var config = nativeSettings.getSaveConfig$intellij_android_adb_ui();
            config.setSaveLocation(directory.toString());
            assertEquals(directory.toString(), LogcatScreenshotSettings.saveLocation());
            assertEquals(directory, LogcatScreenshotSettings.resolveDirectory(project, LogcatScreenshotSettings.saveLocation()));
            config.setSaveLocation(directory.resolve("changed").toString());
            assertEquals(directory.resolve("changed"), LogcatScreenshotSettings.resolveDirectory(project, LogcatScreenshotSettings.saveLocation()));
            var home = Path.of(SystemProperties.getUserHome());
            assertEquals(home, LogcatScreenshotSettings.resolveDirectory(project, ""));
            assertEquals(home.resolve("relative"), LogcatScreenshotSettings.resolveDirectory(project, "relative"));
            assertEquals(home.resolve("pictures"), LogcatScreenshotSettings.resolveDirectory(project, "$USER_HOME$/pictures"));
            assertThrows(IllegalArgumentException.class, () -> LogcatScreenshotSettings.resolveDirectory(project, "bad\0path"));
        } finally {
            Disposer.dispose(lifetime);
        }
    }
}
