package privateacceptance.windowsmenu;

import java.util.List;

/** Headless regression: actual row factories and dummy toggle path, no windows/reflection. */
public final class MenuStateTest {
    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }

    private static WindowsMenuPrototype.Row toggle(WindowsMenuPrototype menu, int project) {
        return menu.projectRows(project).stream().filter(row -> row.toggle).findFirst().orElseThrow();
    }

    private static void verify(WindowsMenuPrototype menu, int project, boolean checked) {
        String expected = checked ? "● 取得中" : "○ 取得OFF";
        List<WindowsMenuPrototype.Row> root = menu.rootRows();
        WindowsMenuPrototype.Row projectRow = root.get(project + 2);
        require(projectRow.project == project && projectRow.submenu(), "project identity/order changed");
        require(projectRow.label.endsWith(expected), "stale root label: project=" + project
            + " expected=" + expected + " actual=" + projectRow.label);
        require(menu.projectRows(project).getFirst().label.startsWith(expected), "stale submenu status");
        require(toggle(menu, project).checked == checked, "stale checkbox snapshot");
        require(menu.projectRows(project).stream().anyMatch(row -> "SCREENSHOT".equals(row.command) && !row.enabled),
            "disabled screenshot changed");
    }

    public static void main(String[] args) {
        WindowsMenuPrototype menu = new WindowsMenuPrototype(null);
        verify(menu, 0, true);
        verify(menu, 1, false);
        List<WindowsMenuPrototype.Row> initialRoot = menu.rootRows();
        WindowsMenuPrototype.Row initialToggle = toggle(menu, 0);
        for (int cycle = 0; cycle < 3; cycle++) {
            menu.command(toggle(menu, 0));
            verify(menu, 0, false);
            verify(menu, 1, false);
            menu.command(toggle(menu, 1));
            verify(menu, 0, false);
            verify(menu, 1, true);
            menu.command(toggle(menu, 0));
            verify(menu, 0, true);
            verify(menu, 1, true);
            menu.command(toggle(menu, 1));
            verify(menu, 0, true);
            verify(menu, 1, false);
        }
        require(initialRoot.get(2).label.endsWith("● 取得中") && initialToggle.checked,
            "old row snapshots mutated");
        require(initialRoot != menu.rootRows(), "reopen must build fresh rows");
        System.out.println("PASS: ON/OFF/reopen root, submenu, checkbox; independent projects; 12 toggles; immutable snapshots");
    }
}
