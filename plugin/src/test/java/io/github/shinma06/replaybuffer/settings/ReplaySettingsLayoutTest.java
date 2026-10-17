package io.github.shinma06.replaybuffer.settings;

import com.intellij.mock.MockApplication;
import com.intellij.mock.MockProject;
import com.intellij.openapi.application.ApplicationManager;
import com.intellij.openapi.client.ClientSession;
import com.intellij.openapi.client.ClientSessionsManager;
import com.intellij.openapi.extensions.ExtensionPoint;
import com.intellij.openapi.ui.TextFieldWithBrowseButton;
import com.intellij.openapi.util.Disposer;
import com.intellij.ui.ExperimentalUI;
import com.intellij.ui.ExperimentalUIImpl;
import com.intellij.ui.ScrollPaneFactory;
import com.intellij.ui.components.GradientViewport;
import com.intellij.ui.components.JBTextArea;
import com.intellij.util.ui.JBUI;
import io.github.shinma06.replaybuffer.ide.ReplayProjectService;
import kotlinx.coroutines.CoroutineScope;
import org.junit.jupiter.api.Test;

import java.awt.Component;
import java.awt.Container;
import java.util.ArrayList;
import java.util.List;
import javax.swing.JComponent;
import javax.swing.SwingUtilities;
import javax.swing.text.Utilities;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ReplaySettingsLayoutTest {
    @Test
    void actualSettingsFormShrinksAfterWideningWithShortAndLongDestinations() throws Exception {
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
                for (String destination : List.of("", "/disposable/" + "long-directory/".repeat(12) + "save")) {
                    var saved = new ReplaySettingsState(false, 180, destination, AppSelectionMode.AUTOMATIC, "");
                    store.loadState(saved);
                    var configurable = new ReplaySettingsConfigurable(project);
                    try {
                        var panel = configurable.createComponent();
                        configurable.reset();
                        var guide = (JBTextArea) find(panel, "package名の入力条件");
                        var fields = List.of(find(panel, "保持時間（1〜900秒）"), find(panel, "保存先フォルダ"),
                            find(panel, "対象アプリの選択方法"), find(panel, "対象アプリのpackage名"));
                        assertEquals(guide.getText(), fields.get(3).getAccessibleContext().getAccessibleDescription());
                        assertFalse(guide.isEditable());
                        assertFalse(guide.isFocusable());
                        panel.setBorder(JBUI.Borders.empty(11, 16, 16, 16));
                        var scroll = ScrollPaneFactory.createScrollPane(null, true);
                        scroll.setViewport(new GradientViewport(panel, JBUI.insetsTop(5), true));
                        var reference = new JBTextArea(guide.getText());
                        reference.setFont(guide.getFont());
                        reference.setBorder(guide.getBorder());
                        reference.setMargin(guide.getMargin());
                        reference.setLineWrap(true);
                        reference.setWrapStyleWord(true);
                        int boundary = -1;
                        int previousHeight = naturalHeight(reference, 400);
                        for (int width = 401; width <= 1800; width++) {
                            int height = naturalHeight(reference, width);
                            if (height < previousHeight) boundary = width;
                            previousHeight = height;
                        }
                        assertTrue(boundary > 0, "the actual font must have a measured wrap transition");
                        int padding = panel.getInsets().left + panel.getInsets().right;
                        int wide = boundary + padding;
                        int narrow = wide - 1;
                        var rowEnds = new ArrayList<Integer>();
                        var heights = new ArrayList<Integer>();
                        for (int width : new int[]{wide, narrow, wide, narrow, 600, 800, 600}) {
                            scroll.setSize(width, 650);
                            layout(scroll);
                            int viewportWidth = scroll.getViewport().getExtentSize().width;
                            assertEquals(viewportWidth, panel.getWidth(), "form width after resize to " + width);
                            assertFalse(scroll.getHorizontalScrollBar().isVisible(), "unexpected horizontal overflow");
                            var components = new ArrayList<>(fields);
                            components.add(guide);
                            for (var component : components) {
                                var bounds = SwingUtilities.convertRectangle(component.getParent(), component.getBounds(), panel);
                                assertTrue(bounds.x >= 0 && bounds.width > 0 && bounds.x + bounds.width <= viewportWidth,
                                    "field/guide must fit after resize to " + width + ": " + bounds);
                            }
                            try {
                                for (int offset = 0; offset < guide.getText().length(); offset++) {
                                    var character = guide.modelToView2D(offset);
                                    assertTrue(character.getX() >= 0 && character.getMaxX() <= guide.getWidth(),
                                        "character " + offset + " must fit horizontally: " + character + ", " + guide.getSize());
                                    assertTrue(character.getY() >= 0 && character.getMaxY() <= guide.getHeight(),
                                        "character " + offset + " needs enough wrapped height: " + character + ", " + guide.getSize());
                                }
                                rowEnds.add(Utilities.getRowEnd(guide, 0));
                            } catch (javax.swing.text.BadLocationException e) {
                                throw new AssertionError(e);
                            }
                            heights.add(guide.getHeight());
                        }
                        assertEquals(rowEnds.get(0), rowEnds.get(2), "wide wrapping must recover");
                        assertEquals(rowEnds.get(1), rowEnds.get(3), "boundary wrapping must recover");
                        assertTrue(rowEnds.get(0) > rowEnds.get(1), "the measured boundary must change wrapping: " + rowEnds);
                        assertEquals(heights.get(0), heights.get(2), "wide height must recover");
                        assertEquals(heights.get(1), heights.get(3), "boundary height must recover");
                        assertTrue(heights.get(0) < heights.get(1), "the measured boundary must add a line: " + heights);
                        assertEquals(rowEnds.get(4), rowEnds.get(6), "600px wrapping must recover");
                        assertEquals(heights.get(4), heights.get(6), "600px height must recover");
                        // Both fixed widths can already fit the full guidance with the current font.
                        assertTrue(rowEnds.get(5) >= rowEnds.get(4), "800px guidance must not wrap earlier: " + rowEnds);
                        assertEquals(destination, ((TextFieldWithBrowseButton) fields.get(1)).getText());
                        assertFalse(configurable.isModified());
                        assertEquals(saved, store.getState(), "layout must not apply settings");
                    } finally {
                        configurable.disposeUIResources();
                    }
                }
            });
        } finally {
            Disposer.dispose(root);
        }
    }

    private static JComponent find(Container parent, String name) {
        for (Component child : parent.getComponents()) {
            if (child instanceof JComponent component && component.getAccessibleContext() != null
                && name.equals(component.getAccessibleContext().getAccessibleName())) return component;
            if (child instanceof Container container) {
                var found = find(container, name);
                if (found != null) return found;
            }
        }
        return null;
    }

    private static int naturalHeight(JBTextArea reference, int width) {
        reference.setSize(width, Integer.MAX_VALUE);
        return reference.getUI().getPreferredSize(reference).height;
    }

    private static void layout(Container parent) {
        try {
            var validateTree = Container.class.getDeclaredMethod("validateTree");
            validateTree.setAccessible(true);
            synchronized (parent.getTreeLock()) {
                validateTree.invoke(parent);
            }
        } catch (ReflectiveOperationException e) {
            throw new AssertionError(e);
        }
    }
}
