package io.github.shinma06.replaybuffer.ui;

import com.android.tools.idea.gradle.project.sync.GradleSyncState;
import com.intellij.mock.MockApplication;
import com.intellij.mock.MockProject;
import com.intellij.notification.NotificationDisplayType;
import com.intellij.notification.NotificationGroup;
import com.intellij.notification.NotificationGroupManager;
import com.intellij.openapi.Disposable;
import com.intellij.openapi.application.ApplicationManager;
import com.intellij.openapi.client.ClientSession;
import com.intellij.openapi.client.ClientSessionsManager;
import com.intellij.openapi.extensions.ExtensionPoint;
import com.intellij.openapi.project.Project;
import com.intellij.openapi.util.Disposer;
import com.intellij.ui.ExperimentalUI;
import com.intellij.ui.ExperimentalUIImpl;
import io.github.shinma06.replaybuffer.core.ApplicationTarget;
import io.github.shinma06.replaybuffer.core.AutoSaveSnapshot;
import io.github.shinma06.replaybuffer.core.CaptureMode;
import io.github.shinma06.replaybuffer.core.CaptureState;
import io.github.shinma06.replaybuffer.core.ReplaySettings;
import io.github.shinma06.replaybuffer.core.ReplaySnapshot;
import io.github.shinma06.replaybuffer.core.SaveSnapshot;
import io.github.shinma06.replaybuffer.core.StreamSnapshot;
import io.github.shinma06.replaybuffer.ide.ReplayCaptureView;
import io.github.shinma06.replaybuffer.ide.ReplayProjectService;
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore;
import kotlinx.coroutines.CoroutineScope;
import kotlinx.coroutines.CoroutineScopeKt;
import kotlinx.coroutines.SupervisorKt;
import org.junit.jupiter.api.Test;

import java.awt.Component;
import java.awt.Container;
import java.awt.Point;
import java.awt.event.ActionEvent;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import javax.swing.AbstractButton;
import javax.swing.JComponent;
import javax.swing.JLabel;
import javax.swing.JPanel;
import javax.swing.JScrollPane;
import javax.swing.JTextArea;
import javax.swing.KeyStroke;
import javax.swing.SwingUtilities;
import javax.swing.text.DefaultEditorKit;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ReplayToolWindowLayoutTest {
    @Test
    void actualToolWindowReflowsAfterWideningAndKeepsTextEndAndControlsReachable() throws Exception {
        var lifetime = Disposer.newDisposable("tool window layout test");
        var application = new MockApplication(lifetime);
        ApplicationManager.setApplication(application, lifetime);
        application.registerService(CoroutineScope.class, application.getCoroutineScope());
        application.registerService(ClientSessionsManager.class,
            new ClientSessionsManager<ClientSession>(application.getCoroutineScope()));
        application.getExtensionArea().registerExtensionPoint(
            "com.intellij.iconMapperSuppressor", Object.class.getName(), ExtensionPoint.Kind.INTERFACE, false);
        application.registerService(ExperimentalUI.class, new ExperimentalUIImpl());
        application.registerService(NotificationGroupManager.class, new NotificationGroupManager() {
            public NotificationGroup getNotificationGroup(String id) {
                return NotificationGroup.create(id, NotificationDisplayType.NONE, false, null, null,
                    com.intellij.openapi.extensions.PluginId.getId("com.intellij"));
            }
            public boolean isGroupRegistered(String id) { return false; }
            public java.util.Collection<NotificationGroup> getRegisteredNotificationGroups() { return List.of(); }
            public boolean isRegisteredNotificationId(String id) { return false; }
        });
        var project = new MockProject(null, lifetime);
        var store = new ReplaySettingsStore();
        var originalSettings = store.getState();
        project.registerService(ReplaySettingsStore.class, store);
        project.registerService(GradleSyncState.class, (GradleSyncState) java.lang.reflect.Proxy.newProxyInstance(
            GradleSyncState.class.getClassLoader(), new Class<?>[]{GradleSyncState.class}, (proxy, method, args) -> {
                if (method.getName().equals("subscribe")) return project.getMessageBus().connect((Disposable) args[2]);
                throw new AssertionError("Unexpected Gradle operation: " + method.getName());
            }));
        // A cancelled scope constructs the real service without SDK resolution or acquisition.
        var job = SupervisorKt.SupervisorJob(null);
        job.cancel(null);
        var service = new ReplayProjectService(project, CoroutineScopeKt.CoroutineScope(job));
        project.registerService(ReplayProjectService.class, service);
        Disposer.register(lifetime, service);
        var actual = new JPanel[1];
        try {
            SwingUtilities.invokeAndWait(() -> {
                try {
                    var type = Class.forName("io.github.shinma06.replaybuffer.ui.ReplayToolWindowPanel");
                    var constructor = type.getDeclaredConstructor(Project.class);
                    constructor.setAccessible(true);
                    var panel = (JPanel) constructor.newInstance(project);
                    actual[0] = panel;
                    Disposer.register(lifetime, (Disposable) panel);
                    verifyScreenshotAvailability(panel, service, store);
                    var outer = (JScrollPane) panel.getComponent(0);
                    var summary = (JTextArea) field(panel, "autoSave");
                    var summaryScroll = (JScrollPane) SwingUtilities.getAncestorOfClass(JScrollPane.class, summary);
                    var path = "/disposable/" + "long-directory-without-spaces/".repeat(16) + "saved-result";
                    var destination = field(panel, "destination");
                    if (destination instanceof JLabel label) label.setText("保存先: " + path);
                    else if (destination instanceof JTextArea area) area.setText("保存先: " + path);
                    else throw new AssertionError("Missing actual destination component");
                    var text = "クラッシュ自動保存: 設定ON / 照合済みの報告を待っています。\n" +
                        "受付可能: 残り8回 / 保存しなかった報告: 1件\n" +
                        "要確認: 対象アプリの報告を照合できないため保存していません。\n" +
                        "保存先: " + path + "\nnative/ANRは対応外。報告時刻は死亡時刻ではありません。文章末尾";
                    summary.setText(text);
                    assertEquals("クラッシュ自動保存の適用状態と直近の結果", summary.getAccessibleContext().getAccessibleName());
                    assertFalse(summary.isEditable());
                    ((JPanel) field(panel, "failureCard")).setVisible(true);
                    var controls = new ArrayList<AbstractButton>();
                    for (var name : List.of("enabled", "settingsButton", "screenshotButton", "saveButton", "openButton",
                        "retryButton", "retryElsewhere", "discardButton")) {
                        var control = (AbstractButton) field(panel, name);
                        control.setEnabled(true);
                        control.setVisible(true);
                        controls.add(control);
                    }
                    var focusable = controls.stream().map(Component::isFocusable).toList();
                    var traversal = controls.stream().map(Component::getFocusTraversalKeysEnabled).toList();
                    var endHeights = new ArrayList<Integer>();
                    for (int width : new int[]{900, 280, 900, 280}) {
                        panel.setSize(width, 440);
                        layout(panel);
                        // Wrapped text revalidates after receiving its new viewport width.
                        layout(panel);
                        var form = (JComponent) outer.getViewport().getView();
                        assertEquals(outer.getViewport().getExtentSize().width, form.getWidth(), "outer form width at " + width);
                        assertFalse(outer.getHorizontalScrollBar().isVisible(), "outer horizontal overflow at " + width);
                        var summaryBounds = SwingUtilities.convertRectangle(summaryScroll.getParent(), summaryScroll.getBounds(), form);
                        assertTrue(summaryBounds.x >= 0 && summaryBounds.width > 0 && summaryBounds.getMaxX() <= form.getWidth(),
                            "summary viewport must fit actual form: " + summaryBounds + " / " + form.getSize());
                        assertEquals(summaryScroll.getViewport().getExtentSize().width, summary.getWidth());
                        for (int offset = 0; offset < text.length(); offset++) {
                            var character = summary.modelToView2D(offset);
                            assertTrue(character.getX() >= 0 && character.getMaxX() <= summary.getWidth(),
                                "summary character " + offset + " outside width: " + character + " / " + summary.getSize());
                        }
                        var end = summary.modelToView2D(text.length()).getBounds();
                        endHeights.add(end.y);
                        var endAction = summary.getActionMap().get(DefaultEditorKit.endAction);
                        assertNotNull(endAction);
                        endAction.actionPerformed(new ActionEvent(summary, ActionEvent.ACTION_PERFORMED, "end"));
                        assertEquals(text.length(), summary.getCaretPosition());
                        summary.scrollRectToVisible(end);
                        assertTrue(summaryScroll.getViewport().getViewRect().contains(end), "full summary end must be reachable at " + width + ": end=" + end + ", visible=" +
                                summaryScroll.getViewport().getViewRect() + ", text=" + summary.getSize());
                        assertEquals(text, summary.getText());
                        var pathLabel = (JTextArea) destination;
                        for (int offset = 0; offset < pathLabel.getText().length(); offset++) {
                            var character = pathLabel.modelToView2D(offset);
                            assertTrue(character.getX() >= 0 && character.getMaxX() <= pathLabel.getWidth()
                                && character.getY() >= 0 && character.getMaxY() <= pathLabel.getHeight(),
                                "destination must wrap without clipped text at " + width + ": " + character + " / " + pathLabel.getSize());
                        }
                        var pathEnd = pathLabel.modelToView2D(pathLabel.getText().length()).getBounds();
                        pathLabel.scrollRectToVisible(pathEnd);
                        assertTrue(outer.getViewport().getViewRect().contains(
                            SwingUtilities.convertRectangle(pathLabel, pathEnd, form)), "full path end must be reachable vertically");
                        assertEquals("保存先: " + path, pathLabel.getText());
                        for (var control : controls) {
                            var bounds = SwingUtilities.convertRectangle(control.getParent(), control.getBounds(), form);
                            assertTrue(bounds.x >= 0 && bounds.width > 0 && bounds.getMaxX() <= form.getWidth(),
                                "control " + control.getText() + " must fit at " + width + ": " + bounds + " / " + form.getSize());
                            form.scrollRectToVisible(bounds);
                            assertTrue(outer.getViewport().getViewRect().contains(bounds), "control " + control.getText() + " must be reachable");
                            assertEquals(focusable.get(controls.indexOf(control)), control.isFocusable(), "platform focusability is retained");
                            assertEquals(traversal.get(controls.indexOf(control)), control.getFocusTraversalKeysEnabled(),
                                "platform focus traversal is retained");
                            assertNotNull(control.getInputMap(JComponent.WHEN_FOCUSED).get(KeyStroke.getKeyStroke("SPACE")));
                            assertTrue(control.getActionListeners().length > 0);
                        }
                    }
                    assertTrue(endHeights.get(1) > endHeights.get(0), "narrow width must add wrapped lines");
                    assertEquals(endHeights.get(0), endHeights.get(2), "wide wrapping must recover");
                    assertEquals(endHeights.get(1), endHeights.get(3), "narrow wrapping must recover");
                    assertFalse(store.getEnabled(), "layout does not enable acquisition");
                    assertEquals(originalSettings, store.getState(), "layout does not apply settings");
                } catch (ReflectiveOperationException | javax.swing.text.BadLocationException failure) {
                    throw new AssertionError(failure);
                }
            });
            SwingUtilities.invokeAndWait(() -> refreshSnapshot(actual[0], service, 1));
            // Observe after the document-update caret callbacks queued by the previous EDT turn.
            SwingUtilities.invokeAndWait(() -> {});
            for (int position : new int[]{0, 1}) {
                var selected = new Point();
                SwingUtilities.invokeAndWait(() -> {
                    var viewport = ((JScrollPane) actual[0].getComponent(0)).getViewport();
                    var scrollableHeight = viewport.getViewSize().height - viewport.getExtentSize().height;
                    if (position != 0) assertTrue(scrollableHeight > 1, "fixture needs a nonzero interior scroll position");
                    selected.y = position == 0 ? 0 : scrollableHeight / 2;
                    viewport.setViewPosition(selected);
                    assertEquals(selected, viewport.getViewPosition(), "fixture must select a reachable scroll position");
                    refreshSnapshot(actual[0], service, position + 2);
                });
                SwingUtilities.invokeAndWait(() -> assertEquals(selected,
                    ((JScrollPane) actual[0].getComponent(0)).getViewport().getViewPosition(),
                    "changing snapshot must preserve the selected viewport after queued caret work"));
            }
        } finally {
            Disposer.dispose(lifetime);
        }
    }

    private static void verifyScreenshotAvailability(JPanel panel, ReplayProjectService service,
                                                     ReplaySettingsStore store) throws ReflectiveOperationException {
        var button = (AbstractButton) field(panel, "screenshotButton");
        assertFalse(button.isEnabled(), "unresolved application cannot capture another app");
        var environment = service.getClass().getDeclaredField("environment");
        environment.setAccessible(true);
        environment.set(service, new io.github.shinma06.replaybuffer.ide.AndroidReplayEnvironment(
            Path.of("/disposable/adb"), null,
            new io.github.shinma06.replaybuffer.ide.ApplicationSelection("sample.app", "sample", null)));
        var settings = service.getClass().getDeclaredField("screenshotSettings");
        settings.setAccessible(true);
        settings.set(service, new io.github.shinma06.replaybuffer.settings.ReplaySettings(180, "/disposable",
            io.github.shinma06.replaybuffer.settings.AppSelectionMode.AUTOMATIC, "", CaptureMode.VIDEO_AND_LOGS, false));
        var revision = service.getClass().getDeclaredField("screenshotRevision");
        revision.setAccessible(true);
        revision.set(service, field(service, "requestRevision"));
        var render = panel.getClass().getDeclaredMethod("render");
        render.setAccessible(true);
        render.invoke(panel);
        assertTrue(button.isEnabled(), "screenshot is available with replay OFF and capture initialization pending");
        var busy = service.getClass().getDeclaredField("screenshotBusy");
        busy.setAccessible(true);
        busy.setBoolean(service, true);
        render.invoke(panel);
        assertFalse(button.isEnabled(), "overlapping screenshot requests are disabled");
        busy.setBoolean(service, false);
        assertFalse(store.getEnabled(), "screenshot must not enable replay acquisition");
        revision.setLong(service, -1L);
        render.invoke(panel);
        assertFalse(button.isEnabled(), "stale selection cannot capture the previously selected app");
    }

    private static void refreshSnapshot(JPanel panel, ReplayProjectService service, long revision) {
        try {
            var settings = new ReplaySettings(null, Path.of("/disposable/" + "long-directory/".repeat(16)),
                180, new ApplicationTarget(), CaptureMode.VIDEO_AND_LOGS, false);
            var snapshot = new ReplaySnapshot(revision, 1, true, CaptureState.CAPTURING, settings, 1,
                null, "layout-test", revision * 1_000_000_000L, false,
                new StreamSnapshot(), new StreamSnapshot(), new StreamSnapshot(), new SaveSnapshot(),
                false, null, null, 0L, 0, new AutoSaveSnapshot(), null);
            var capture = field(service, "capture");
            var view = capture.getClass().getDeclaredField("view");
            view.setAccessible(true);
            view.set(capture, new ReplayCaptureView(snapshot, 0, null, null));
            var window = (JTextArea) field(panel, "window");
            var previousText = window.getText();
            var render = panel.getClass().getDeclaredMethod("render");
            render.setAccessible(true);
            render.invoke(panel);
            layout(panel);
            layout(panel);
            assertFalse(previousText.equals(window.getText()), "the snapshot refresh must really change displayed text");
        } catch (ReflectiveOperationException failure) {
            throw new AssertionError(failure);
        }
    }

    private static Object field(Object owner, String name) throws ReflectiveOperationException {
        var field = owner.getClass().getDeclaredField(name);
        field.setAccessible(true);
        return field.get(owner);
    }

    private static void layout(Container parent) throws ReflectiveOperationException {
        var validate = Container.class.getDeclaredMethod("validateTree");
        validate.setAccessible(true);
        synchronized (parent.getTreeLock()) { validate.invoke(parent); }
    }
}
