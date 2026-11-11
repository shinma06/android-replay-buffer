package io.github.shinma06.replaybuffer.settings;

/** Drives the Settings draft without a Compose window, the same way user edits do. */
public final class SettingsFormTestSupport {
    private SettingsFormTestSupport() {}

    public static SettingsForm createForm(Object configurable) throws ReflectiveOperationException {
        for (var method : configurable.getClass().getDeclaredMethods()) {
            if (method.getName().startsWith("createForm") && method.getParameterCount() == 0
                && method.getReturnType() == SettingsForm.class) {
                method.setAccessible(true);
                return (SettingsForm) method.invoke(configurable);
            }
        }
        throw new AssertionError("createForm");
    }

    public static void edited(Object configurable) throws ReflectiveOperationException {
        var method = configurable.getClass().getDeclaredMethod("onFormEdited");
        method.setAccessible(true);
        method.invoke(configurable);
    }

    public static void setText(androidx.compose.foundation.text.input.TextFieldState state, String text) {
        androidx.compose.foundation.text.input.TextFieldStateKt.setTextAndPlaceCursorAtEnd(state, text);
    }
}
