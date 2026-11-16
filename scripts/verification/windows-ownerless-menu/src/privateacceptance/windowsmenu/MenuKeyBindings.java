package privateacceptance.windowsmenu;

import javax.swing.Action;
import javax.swing.JComponent;
import javax.swing.KeyStroke;

/** Component-local menu bindings; no event dispatcher or focus changes. */
final class MenuKeyBindings {
    private MenuKeyBindings() {}

    static void bind(JComponent component, String key, Action action) {
        component.getInputMap(JComponent.WHEN_FOCUSED).put(KeyStroke.getKeyStroke(key), key);
        component.getActionMap().put(key, action);
        // The IDE handles list arrows before Swing key lookup and invokes these standard
        // JList actions directly. Both routes must run the same menu command (including
        // skipping disabled rows), rather than the look-and-feel's default list movement.
        String listAction = switch (key) {
            case "UP" -> "selectPreviousRow";
            case "DOWN" -> "selectNextRow";
            case "RIGHT" -> "selectNextColumn";
            case "LEFT" -> "selectPreviousColumn";
            default -> null;
        };
        if (listAction != null) component.getActionMap().put(listAction, action);
    }
}
