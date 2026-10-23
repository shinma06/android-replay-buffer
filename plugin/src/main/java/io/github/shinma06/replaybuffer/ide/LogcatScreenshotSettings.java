package io.github.shinma06.replaybuffer.ide;

import com.android.tools.idea.ui.save.SaveConfigurationResolver;
import com.android.tools.idea.ui.screenshot.DeviceScreenshotSettings;
import com.intellij.openapi.project.Project;
import java.nio.file.Path;
import java.time.Instant;

/** The supported Studio exposes its Kotlin internal screenshot settings through this JVM ABI. */
public final class LogcatScreenshotSettings {
    private LogcatScreenshotSettings() {}

    // shortcut: uses the supported Studio internal JVM ABI; recheck when updating Studio.
    public static String saveLocation() {
        return DeviceScreenshotSettings.getInstance().getSaveConfig$intellij_android_adb_ui().getSaveLocation();
    }

    /** Use Studio's entire location resolution, including relative/home and OS Desktop semantics. Call off EDT. */
    public static Path resolveDirectory(Project project, String location) {
        var filename = project.getService(SaveConfigurationResolver.class)
            .expandFilenamePattern(location, "screenshot", "png", Instant.EPOCH, 1);
        if (filename.isEmpty()) throw new IllegalArgumentException("Logcatのスクショ保存先のパスを確認してください。");
        return Path.of(filename).getParent();
    }
}
