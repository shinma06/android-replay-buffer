package privateacceptance.windowsmenu;

import java.awt.event.ActionEvent;
import java.awt.event.KeyEvent;
import java.util.LinkedHashMap;
import java.util.Map;
import javax.swing.*;

/** Actual binder, exercised both through Swing key lookup and the IDE's observed list-action route. */
public final class MenuKeyBindingsTest {
    private record Route(String key, String listAction, String command) {}
    // These Swing action names are invoked directly by the fixed SDK's ListActions delegates.
    private static final Route[] ROUTES = {
        new Route("RIGHT", "selectNextColumn", "open"),
        new Route("LEFT", "selectPreviousColumn", "parent"),
        new Route("UP", "selectPreviousRow", "up"),
        new Route("DOWN", "selectNextRow", "down"),
        new Route("ENTER", null, "activate"),
        new Route("ESCAPE", null, "close"),
        new Route("SPACE", null, "activate-space")
    };

    private static void require(boolean ok, String message) {
        if (!ok) throw new AssertionError(message);
    }

    private static final class TestList extends JList<String> {
        TestList() { super(new String[] {"project", "command"}); }
        boolean press(String key) {
            KeyStroke stroke = KeyStroke.getKeyStroke(key);
            KeyEvent event = new KeyEvent(this, KeyEvent.KEY_PRESSED, 0, 0, stroke.getKeyCode(), KeyEvent.CHAR_UNDEFINED);
            return processKeyBinding(stroke, event, JComponent.WHEN_FOCUSED, true);
        }
    }

    private static void checkMenu(String level) {
        TestList list = new TestList();
        Map<String,Integer> calls = new LinkedHashMap<>();
        for (Route route : ROUTES) {
            calls.put(route.command, 0);
            MenuKeyBindings.bind(list, route.key, new AbstractAction() {
                @Override public void actionPerformed(ActionEvent e) {
                    calls.compute(route.command, (k, count) -> count + 1);
                }
            });
        }
        for (Route route : ROUTES) {
            require(list.press(route.key), level + " " + route.key + " Swing key route not handled");
            require(calls.get(route.command) == 1, level + " " + route.key + " wrong Swing callback");
            if (route.listAction != null) {
                Action delegate = list.getActionMap().get(route.listAction);
                require(delegate != null, "fixed SDK consumer's Swing action missing: " + route.listAction);
                delegate.actionPerformed(new ActionEvent(list, ActionEvent.ACTION_PERFORMED, route.listAction));
                require(calls.get(route.command) == 2, level + " " + route.key
                    + " IDE list-action route bypassed menu callback: " + route.listAction);
            }
        }
        // The next menu gets separate closures/maps: child LEFT cannot run the root's callback.
        TestList other = new TestList();
        int[] childParent = {0};
        MenuKeyBindings.bind(other, "LEFT", new AbstractAction() {
            @Override public void actionPerformed(ActionEvent e) { childParent[0]++; }
        });
        require(other.press("LEFT") && childParent[0] == 1, "child parent callback missing");
        require(calls.get("parent") == 2, "child navigation leaked into another menu");
    }

    public static void main(String[] args) throws Exception {
        SwingUtilities.invokeAndWait(() -> {
            checkMenu("root");
            checkMenu("child");
        });
        System.out.println("PASS: root/child RIGHT-open LEFT-parent UP DOWN ENTER ESCAPE SPACE; Swing and IDE list-action routes");
    }
}
