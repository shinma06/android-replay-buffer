package io.github.shinma06.arb.ide;

import com.intellij.openapi.project.Project;
import java.lang.reflect.Method;
import java.nio.file.InvalidPathException;
import java.nio.file.Path;
import java.time.Instant;
import org.jetbrains.annotations.Nullable;

/**
 * Safely queries Android Studio's Logcat / Device Screenshot destination settings via reflection
 * so `ide-plugin` compiles and runs cleanly across Android Studio ABI changes and non-Studio
 * IntelliJ hosts (returning `null` so callers fall back cleanly to `output_dir`).
 */
public final class LogcatScreenshotSettings {
    private static final String DEVICE_SCREENSHOT_SETTINGS_CLASS =
        "com.android.tools.idea.ui.screenshot.DeviceScreenshotSettings";
    private static final String SAVE_CONFIG_RESOLVER_CLASS =
        "com.android.tools.idea.ui.save.SaveConfigurationResolver";

    private LogcatScreenshotSettings() {}

    /**
     * Returns Android Studio's configured raw screenshot save location pattern (e.g. `$DESKTOP_DIR$`),
     * or `null` if running outside Android Studio or if Studio's internal ABI is unavailable.
     */
    @Nullable
    public static String saveLocationOrNull() {
        try {
            Class<?> settingsCls = Class.forName(DEVICE_SCREENSHOT_SETTINGS_CLASS);
            Method getInstance = settingsCls.getMethod("getInstance");
            Object instance = getInstance.invoke(null);
            if (instance == null) {
                return null;
            }
            Object saveConfig = invokeSaveConfig(instance);
            if (saveConfig == null) {
                return null;
            }
            Method getSaveLocation = saveConfig.getClass().getMethod("getSaveLocation");
            Object value = getSaveLocation.invoke(saveConfig);
            return (value instanceof String) ? (String) value : null;
        } catch (Exception | LinkageError ignored) {
            return null;
        }
    }

    /**
     * Resolves a raw Android Studio screenshot save location string (including `$DESKTOP_DIR$`,
     * `$USER_HOME$`, `$PROJECT_DIR$`, or relative paths) into an absolute local directory [Path],
     * or `null` when it cannot be resolved safely. Call off the EDT when passing a live [Project].
     */
    @Nullable
    public static Path resolveDirectoryOrNull(@Nullable Project project, @Nullable String location) {
        if (location == null) {
            return null;
        }
        if (project != null && !project.isDisposed()) {
            Path resolvedViaStudio = resolveViaStudioService(project, location);
            if (resolvedViaStudio != null) {
                return resolvedViaStudio;
            }
        }
        return resolveFallbackMacros(project, location);
    }

    @Nullable
    private static Object invokeSaveConfig(Object instance) throws ReflectiveOperationException {
        Class<?> cls = instance.getClass();
        String[] candidates = {
            "getSaveConfig$intellij_android_adb_ui",
            "getSaveConfig",
        };
        for (String name : candidates) {
            try {
                Method m = cls.getMethod(name);
                return m.invoke(instance);
            } catch (NoSuchMethodException ignored) {
                // try next candidate
            }
        }
        for (Method m : cls.getMethods()) {
            if (m.getParameterCount() == 0 && m.getName().startsWith("getSaveConfig")) {
                return m.invoke(instance);
            }
        }
        return null;
    }

    @Nullable
    private static Path resolveViaStudioService(Project project, String location) {
        try {
            Class<?> resolverCls = Class.forName(SAVE_CONFIG_RESOLVER_CLASS);
            Object resolver = project.getService(resolverCls);
            if (resolver == null) {
                return null;
            }
            Method expand = resolverCls.getMethod(
                "expandFilenamePattern",
                String.class,
                String.class,
                String.class,
                Instant.class,
                int.class
            );
            Object expanded = expand.invoke(resolver, location, "screenshot", "png", Instant.EPOCH, 1);
            if (!(expanded instanceof String filename) || filename.isEmpty()) {
                return null;
            }
            return Path.of(filename).toAbsolutePath().normalize().getParent();
        } catch (Exception | LinkageError ignored) {
            return null;
        }
    }

    @Nullable
    static Path resolveFallbackMacros(@Nullable Project project, String rawLocation) {
        String trimmed = rawLocation.trim();
        String homeStr = System.getProperty("user.home");
        Path home = (homeStr != null && !homeStr.isBlank()) ? Path.of(homeStr).toAbsolutePath().normalize() : null;
        if (trimmed.isEmpty()) {
            return home;
        }
        String expanded = trimmed;
        if (home != null) {
            expanded = expanded
                .replace("$USER_HOME$", home.toString())
                .replace("$DESKTOP_DIR$", home.resolve("Desktop").toString());
        }
        if (project != null) {
            String basePath = project.getBasePath();
            if (basePath != null && !basePath.isBlank()) {
                expanded = expanded.replace("$PROJECT_DIR$", basePath);
            }
        }
        if (expanded.contains("$")) {
            return null;
        }
        try {
            Path path = Path.of(expanded);
            if (!path.isAbsolute()) {
                if (home == null) {
                    return null;
                }
                path = home.resolve(path);
            }
            return path.toAbsolutePath().normalize();
        } catch (InvalidPathException ignored) {
            return null;
        }
    }
}
