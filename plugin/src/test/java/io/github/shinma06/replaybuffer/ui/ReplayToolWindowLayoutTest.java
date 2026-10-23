package io.github.shinma06.replaybuffer.ui;

import com.android.tools.idea.gradle.project.sync.GradleSyncState;
import com.intellij.mock.MockApplication;
import com.intellij.mock.MockProject;
import com.intellij.notification.NotificationDisplayType;
import com.intellij.notification.NotificationGroup;
import com.intellij.notification.NotificationGroupManager;
import com.intellij.openapi.Disposable;
import com.intellij.openapi.application.ApplicationManager;
import com.intellij.openapi.application.ModalityState;
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
import io.github.shinma06.replaybuffer.core.DeviceKind;
import io.github.shinma06.replaybuffer.core.ReplayDevice;
import io.github.shinma06.replaybuffer.core.ReplaySettings;
import io.github.shinma06.replaybuffer.core.ReplaySnapshot;
import io.github.shinma06.replaybuffer.core.SaveSnapshot;
import io.github.shinma06.replaybuffer.core.StreamSnapshot;
import io.github.shinma06.replaybuffer.ide.ReplayCaptureView;
import io.github.shinma06.replaybuffer.ide.ReplayProjectService;
import io.github.shinma06.replaybuffer.settings.ReplaySettingsStore;
import kotlinx.coroutines.CoroutineScope;
import kotlinx.coroutines.CoroutineScopeKt;
import kotlinx.coroutines.Job;
import kotlinx.coroutines.SupervisorKt;
import org.junit.jupiter.api.Test;

import java.awt.Component;
import java.awt.Container;
import java.awt.Point;
import java.awt.event.ActionEvent;
import java.nio.file.Path;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentLinkedQueue;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
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
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ReplayToolWindowLayoutTest {
    @Test
    void actualToolWindowReflowsAfterWideningAndKeepsTextEndAndControlsReachable() throws Exception {
        var lifetime = Disposer.newDisposable("tool window layout test");
        var uiCallbacks = new ConcurrentLinkedQueue<Runnable>();
        var logcatUnavailable = new AtomicBoolean(true);
        var application = new MockApplication(lifetime) {
            @Override
            public <T> T getService(Class<T> serviceClass) {
                if (serviceClass == com.android.tools.idea.ui.screenshot.DeviceScreenshotSettings.class && logcatUnavailable.get()) return null;
                return super.getService(serviceClass);
            }
            @Override
            public void invokeLater(Runnable action, ModalityState modality) { uiCallbacks.add(action); }
        };
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
            verifyScreenshotSettingsApplyAndCancel(project, service, store);
            SwingUtilities.invokeAndWait(() -> {
                try {
                    verifyScreenshotRequestKeepsTargetAcrossOffPublication(project, service, store, uiCallbacks, logcatUnavailable);
                    var type = Class.forName("io.github.shinma06.replaybuffer.ui.ReplayToolWindowPanel");
                    var constructor = type.getDeclaredConstructor(Project.class);
                    constructor.setAccessible(true);
                    var panel = (JPanel) constructor.newInstance(project);
                    actual[0] = panel;
                    Disposer.register(lifetime, (Disposable) panel);
                    verifyScreenshotAvailability(panel, service, store);
                    verifySaveStatesAndHierarchy(panel, service, store);
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
                    verifyLargeFontPresentation(panel);
                    assertFalse(store.getEnabled(), "layout does not enable acquisition");
                    assertEquals(originalSettings, store.getState(), "layout does not apply settings");
                } catch (Exception failure) {
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

    private static void verifyScreenshotRequestKeepsTargetAcrossOffPublication(
        MockProject project, ReplayProjectService service, ReplaySettingsStore store, ConcurrentLinkedQueue<Runnable> uiCallbacks,
        AtomicBoolean logcatUnavailable
    ) throws Exception {
        var root = Files.createTempDirectory("replay-screenshot-off-boundary-");
        var scope = service.getClass().getDeclaredField("scope");
        scope.setAccessible(true);
        var originalScope = scope.get(service);
        var environment = service.getClass().getDeclaredField("environment");
        environment.setAccessible(true);
        var originalEnvironment = environment.get(service);
        var settings = service.getClass().getDeclaredField("screenshotSettings");
        settings.setAccessible(true);
        var originalSettings = settings.get(service);
        var revision = service.getClass().getDeclaredField("screenshotRevision");
        revision.setAccessible(true);
        var originalRevision = revision.getLong(service);
        var capture = field(service, "capture");
        var view = capture.getClass().getDeclaredField("view");
        view.setAccessible(true);
        var originalView = view.get(capture);
        var requestScope = SupervisorKt.SupervisorJob(null);
        var completed = new CompletableFuture<Void>();
        try {
            var adb = root.resolve("adb");
            Files.writeString(adb, """
                #!/usr/bin/env python3
                import pathlib,sys,time
                root=pathlib.Path(__file__).parent
                command=' '.join(sys.argv[1:])
                with pathlib.Path(__file__).with_name('commands').open('a') as log: log.write(command+'\\n')
                if command=='devices -l':
                 if (root/'pause').exists():
                  (root/'started').touch()
                  deadline=time.monotonic()+5
                  while not (root/'continue').exists():
                   if time.monotonic()>deadline: sys.exit(3)
                   time.sleep(.01)
                 print('List of devices attached\\nusb-B device model:Other')
                elif command=='-s usb-B shell dumpsys SurfaceFlinger --display-id': print('Display 1 (HWC display 0): port=0')
                elif command=='-s usb-B shell dumpsys window': print('KeyguardServiceDelegate\\nshowing=false\\ninputRestricted=false\\ndreaming=false\\nscreenState=SCREEN_STATE_ON\\nDisplay: mDisplayId=0\\nmCurrentFocus=Window{12ab u0 sample.app/.MainActivity}')
                elif command=='-s usb-B exec-out screencap -p -d 1': sys.stdout.buffer.write((root/'image.png').read_bytes() if (root/'image.png').exists() else b'invalid image')
                else: sys.exit(2)
                """);
            assertTrue(adb.toFile().setExecutable(true));
            var applied = new ReplaySettings(adb, root, 180, new ApplicationTarget(), CaptureMode.VIDEO_AND_LOGS, false);
            var active = new ReplaySnapshot(1, 1, true, CaptureState.CAPTURING, applied, 1,
                new ReplayDevice("target-A", "Recording", DeviceKind.PHYSICAL, true), "request-boundary", 1L, false,
                new StreamSnapshot(), new StreamSnapshot(), new StreamSnapshot(), new SaveSnapshot(),
                false, null, null, 0L, 0, new AutoSaveSnapshot(), null);
            var off = new ReplaySnapshot(2, 2, false, CaptureState.DISABLED, applied, 1,
                null, null, null, false, new StreamSnapshot(), new StreamSnapshot(), new StreamSnapshot(),
                new SaveSnapshot(), false, null, null, 0L, 0, new AutoSaveSnapshot(), null);
            view.set(capture, new ReplayCaptureView(active, 0, null, null));
            var publishedOff = new AtomicBoolean();
            var validationPath = (Path) java.lang.reflect.Proxy.newProxyInstance(Path.class.getClassLoader(),
                new Class<?>[]{Path.class}, (proxy, method, arguments) -> {
                    // Publish OFF after validation read A, before the old request's second live snapshot read.
                    if (method.getName().equals("normalize") && publishedOff.compareAndSet(false, true)) {
                        synchronized (capture) { view.set(capture, new ReplayCaptureView(off, 0, null, null)); }
                    }
                    return method.invoke(adb, arguments);
                });
            environment.set(service, new io.github.shinma06.replaybuffer.ide.AndroidReplayEnvironment(validationPath, null,
                new io.github.shinma06.replaybuffer.ide.ApplicationSelection("sample.app", "sample", null)));
            settings.set(service, new io.github.shinma06.replaybuffer.settings.ReplaySettings(180, root.toString(),
                io.github.shinma06.replaybuffer.settings.AppSelectionMode.AUTOMATIC, "", CaptureMode.VIDEO_AND_LOGS, false, false, root.toString()));
            revision.set(service, field(service, "requestRevision"));
            scope.set(service, CoroutineScopeKt.CoroutineScope(requestScope));
            uiCallbacks.clear();
            var request = java.util.Arrays.stream(service.getClass().getDeclaredMethods())
                .filter(method -> method.getName().startsWith("screenshot$") && method.getParameterCount() == 0)
                .findFirst().orElseThrow();
            request.setAccessible(true);
            request.invoke(service);
            var action = (Job) field(service, "screenshotJob");
            if (action == null) completed.complete(null);
            else action.invokeOnCompletion(failure -> { completed.complete(null); return kotlin.Unit.INSTANCE; });
            completed.get(15, TimeUnit.SECONDS);
            assertTrue(publishedOff.get(), "OFF must really be published inside the request validation");
            assertEquals(off, ((ReplayCaptureView) view.get(capture)).getSnapshot());
            assertEquals(List.of("devices -l"), Files.readAllLines(root.resolve("commands")),
                "unavailable A must reject the request without any command targeting B");
            assertEquals(1, uiCallbacks.size(), "the rejected request must schedule its error explanation");
            assertFalse(store.getEnabled(), "screenshot must not enable acquisition");
            assertNull(field(capture, "core"), "screenshot must not start a capture or create history");
            if (System.getProperty("os.name").equals("Mac OS X")) {
                var common = Files.createDirectory(root.resolve("common"));
                var dedicated = Files.createDirectory(root.resolve("screenshots"));
                javax.imageio.ImageIO.write(new java.awt.image.BufferedImage(2, 2, java.awt.image.BufferedImage.TYPE_INT_RGB),
                    "png", root.resolve("image.png").toFile());
                Files.createFile(root.resolve("pause"));
                environment.set(service, new io.github.shinma06.replaybuffer.ide.AndroidReplayEnvironment(adb, null,
                    new io.github.shinma06.replaybuffer.ide.ApplicationSelection("sample.app", "sample", null)));
                settings.set(service, new io.github.shinma06.replaybuffer.settings.ReplaySettings(180, common.toString(),
                    io.github.shinma06.replaybuffer.settings.AppSelectionMode.AUTOMATIC, "", CaptureMode.VIDEO_AND_LOGS,
                    false, false, dedicated.toString()));
                request.invoke(service);
                var saving = (Job) field(service, "screenshotJob");
                var deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5);
                while (!Files.exists(root.resolve("started")) && System.nanoTime() < deadline) Thread.sleep(10);
                assertTrue(Files.exists(root.resolve("started")));
                settings.set(service, new io.github.shinma06.replaybuffer.settings.ReplaySettings(180, common.toString(),
                    io.github.shinma06.replaybuffer.settings.AppSelectionMode.AUTOMATIC, "", CaptureMode.VIDEO_AND_LOGS,
                    false, true, ""));
                Files.createFile(root.resolve("continue"));
                var saved = new CompletableFuture<Void>();
                saving.invokeOnCompletion(failure -> { saved.complete(null); return kotlin.Unit.INSTANCE; });
                saved.get(15, TimeUnit.SECONDS);
                try (var images = Files.list(dedicated); var other = Files.list(common)) {
                    var files = images.toList();
                    assertEquals(1, files.size(), "request must use its captured dedicated root");
                    assertTrue(files.getFirst().getFileName().toString().endsWith(".png"));
                    assertTrue(Files.isRegularFile(files.getFirst()));
                    assertEquals(0, other.count(), "a concurrent settings update must not redirect the request");
                }
                assertFalse(store.getEnabled());
                assertNull(field(capture, "core"));

                var beforeMissingNative = Files.readAllLines(root.resolve("commands"));
                var beforeErrors = uiCallbacks.size();
                var previousJob = field(service, "screenshotJob");
                request.invoke(service);
                assertTrue(previousJob == field(service, "screenshotJob"), "unavailable settings cannot start a screenshot job");
                assertEquals(beforeMissingNative, Files.readAllLines(root.resolve("commands")), "unavailable Logcat settings must not reach adb or replay root");
                assertEquals(beforeErrors + 1, uiCallbacks.size(), "unavailable native settings need an actionable explanation");

                var nativeSettings = new com.android.tools.idea.ui.screenshot.DeviceScreenshotSettings();
                ((MockApplication) ApplicationManager.getApplication()).registerService(
                    com.android.tools.idea.ui.screenshot.DeviceScreenshotSettings.class, nativeSettings);
                project.registerService(com.android.tools.idea.ui.save.SaveConfigurationResolver.class,
                    new com.android.tools.idea.ui.save.SaveConfigurationResolver(project));
                var followed = Files.createDirectory(root.resolve("logcat"));
                var next = Files.createDirectory(root.resolve("logcat-changed"));
                nativeSettings.getSaveConfig$intellij_android_adb_ui().setSaveLocation(followed.toString());
                logcatUnavailable.set(false);
                Files.delete(root.resolve("started"));
                Files.delete(root.resolve("continue"));
                var redirectedBeforeStart = new AtomicBoolean();
                var nativeChange = project.getMessageBus().connect();
                nativeChange.subscribe(ReplayProjectService.Companion.getENVIRONMENT_CHANGED(), (Runnable) () -> {
                    if (redirectedBeforeStart.compareAndSet(false, true))
                        nativeSettings.getSaveConfig$intellij_android_adb_ui().setSaveLocation(next.toString());
                });
                try { request.invoke(service); } finally { nativeChange.disconnect(); }
                assertTrue(redirectedBeforeStart.get(), "native settings must change before the lazy IO job starts");
                saving = (Job) field(service, "screenshotJob");
                assertNotNull(saving);
                deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5);
                while (!Files.exists(root.resolve("started")) && System.nanoTime() < deadline) Thread.sleep(10);
                assertTrue(Files.exists(root.resolve("started")));
                Files.createFile(root.resolve("continue"));
                var logcatSaved = new CompletableFuture<Void>();
                saving.invokeOnCompletion(failure -> { logcatSaved.complete(null); return kotlin.Unit.INSTANCE; });
                logcatSaved.get(15, TimeUnit.SECONDS);
                try (var images = Files.list(followed); var other = Files.list(next); var replay = Files.list(common)) {
                    assertEquals(1, images.count(), "Logcat location must be frozen for the current request");
                    assertEquals(0, other.count());
                    assertEquals(0, replay.count());
                }
                request.invoke(service);
                saving = (Job) field(service, "screenshotJob");
                assertNotNull(saving);
                var nextSaved = new CompletableFuture<Void>();
                saving.invokeOnCompletion(failure -> { nextSaved.complete(null); return kotlin.Unit.INSTANCE; });
                nextSaved.get(15, TimeUnit.SECONDS);
                try (var images = Files.list(next)) {
                    assertEquals(1, images.count(), "the next request must re-read the changed native setting");
                }
                assertFalse(store.getEnabled());
                assertNull(field(capture, "core"));
            }
        } finally {
            requestScope.cancel(null);
            var pending = (Job) field(service, "screenshotJob");
            if (pending != null) {
                var stopped = new CompletableFuture<Void>();
                pending.invokeOnCompletion(failure -> { stopped.complete(null); return kotlin.Unit.INSTANCE; });
                stopped.get(15, TimeUnit.SECONDS);
            }
            scope.set(service, originalScope);
            environment.set(service, originalEnvironment);
            settings.set(service, originalSettings);
            revision.setLong(service, originalRevision);
            view.set(capture, originalView);
            uiCallbacks.clear();
            try (var paths = Files.walk(root)) {
                for (var path : paths.sorted(java.util.Comparator.reverseOrder()).toList()) Files.deleteIfExists(path);
            }
        }
    }

    private static void verifyScreenshotSettingsApplyAndCancel(MockProject project, ReplayProjectService service,
                                                               ReplaySettingsStore store) throws Exception {
        var scope = service.getClass().getDeclaredField("scope");
        scope.setAccessible(true);
        var originalScope = scope.get(service);
        var originalState = store.getState();
        var root = Files.createTempDirectory("replay-screenshot-settings-");
        var configurable = new io.github.shinma06.replaybuffer.settings.ReplaySettingsConfigurable(project);
        ((MockApplication) ApplicationManager.getApplication()).registerService(com.intellij.openapi.application.CoroutineSupport.class,
            new com.intellij.openapi.application.impl.PlatformCoroutineSupport());
        var validationScope = SupervisorKt.SupervisorJob(null);
        scope.set(service, CoroutineScopeKt.CoroutineScope(validationScope));
        try {
            SwingUtilities.invokeAndWait(() -> {
                configurable.createComponent();
                configurable.reset();
                try {
                    assertTrue(((JTextArea) field(configurable, "screenshotStatus")).getText().contains("取得できません"));
                    var logcat = (javax.swing.JCheckBox) field(configurable, "screenshotLogcat");
                    logcat.doClick();
                    org.junit.jupiter.api.Assertions.assertThrows(com.intellij.openapi.options.ConfigurationException.class, configurable::apply);
                    var directory = (com.intellij.openapi.ui.TextFieldWithBrowseButton) field(configurable, "screenshotDirectory");
                    directory.setText(root.toString());
                    org.junit.jupiter.api.Assertions.assertThrows(com.intellij.openapi.options.ConfigurationException.class, configurable::apply,
                        "Apply must not accept an unchecked path");
                } catch (ReflectiveOperationException failure) { throw new AssertionError(failure); }
            });
            var checked = new CompletableFuture<Void>();
            var job = (Job) field(configurable, "screenshotValidationJob");
            assertNotNull(job);
            job.invokeOnCompletion(failure -> {
                if (failure == null) checked.complete(null); else checked.completeExceptionally(failure);
                return kotlin.Unit.INSTANCE;
            });
            checked.get(10, TimeUnit.SECONDS);
            // Restore the cancelled service scope before Apply publishes settings; no SDK resolution is launched.
            scope.set(service, originalScope);
            SwingUtilities.invokeAndWait(() -> {
                try {
                    assertTrue(store.settings().getScreenshotUsesLogcatDestination(), "checking a draft is not Apply");
                    configurable.apply();
                    assertFalse(store.settings().getScreenshotUsesLogcatDestination());
                    assertEquals(root.toString(), store.settings().getScreenshotDirectory());
                    assertEquals("", store.settings().getDestination());
                    assertFalse(configurable.isModified());
                    ((javax.swing.JCheckBox) field(configurable, "screenshotLogcat")).doClick();
                    assertTrue(configurable.isModified());
                    configurable.reset();
                    assertFalse(((javax.swing.JCheckBox) field(configurable, "screenshotLogcat")).isSelected());
                    assertFalse(configurable.isModified());
                    assertEquals(root.toString(), store.settings().getScreenshotDirectory());
                } catch (Exception failure) { throw new AssertionError(failure); }
            });
        } finally {
            validationScope.cancel(null);
            scope.set(service, originalScope);
            SwingUtilities.invokeAndWait(configurable::disposeUIResources);
            store.loadState(originalState);
            Files.delete(root);
        }
    }

    private static void verifySaveStatesAndHierarchy(JPanel panel, ReplayProjectService service,
                                                     ReplaySettingsStore store) throws Exception {
        var capture = field(service, "capture");
        var view = capture.getClass().getDeclaredField("view");
        view.setAccessible(true);
        var originalView = view.get(capture);
        var resolving = service.getClass().getDeclaredField("resolving");
        resolving.setAccessible(true);
        var originalResolving = resolving.getBoolean(service);
        var originalSettings = store.getState();
        var screenshotSettings = service.getClass().getDeclaredField("screenshotSettings");
        screenshotSettings.setAccessible(true);
        var originalScreenshotSettings = screenshotSettings.get(service);
        var screenshotRevision = service.getClass().getDeclaredField("screenshotRevision");
        screenshotRevision.setAccessible(true);
        var originalScreenshotRevision = screenshotRevision.getLong(service);
        var render = panel.getClass().getDeclaredMethod("render");
        render.setAccessible(true);
        var app = new ApplicationTarget("com.fixture.visible", io.github.shinma06.replaybuffer.core.ApplicationMode.MANUAL, null);
        var device = new ReplayDevice("target-A", "Fixture", DeviceKind.EMULATOR, true);
        var settings = new ReplaySettings(Path.of("/disposable/adb"), Path.of("/disposable/configured"),
            30, app, CaptureMode.LOG_ONLY, true);
        var gap = new io.github.shinma06.replaybuffer.core.CaptureGap("clock", null, null,
            "時計対応を確認できません", Long.MAX_VALUE, 0, null);
        var logs = new StreamSnapshot(io.github.shinma06.replaybuffer.core.StreamState.CAPTURING, 0.0, null, List.of(gap));
        var video = new StreamSnapshot(io.github.shinma06.replaybuffer.core.StreamState.NOT_REQUESTED, 0.0, null, List.of());
        var phases = List.of(io.github.shinma06.replaybuffer.core.SavePhase.IDLE,
            io.github.shinma06.replaybuffer.core.SavePhase.WRITING, io.github.shinma06.replaybuffer.core.SavePhase.FAILED,
            io.github.shinma06.replaybuffer.core.SavePhase.COMPLETED, io.github.shinma06.replaybuffer.core.SavePhase.CANCELLED);
        var states = List.of(CaptureState.CAPTURING, CaptureState.CAPTURING, CaptureState.PARTIAL,
            CaptureState.RECOVERING, CaptureState.CAPTURING);
        try {
            resolving.setBoolean(service, false);
            store.loadState(new io.github.shinma06.replaybuffer.settings.ReplaySettingsState(true, 30, "/disposable/configured",
                io.github.shinma06.replaybuffer.settings.AppSelectionMode.MANUAL, "com.fixture.visible", CaptureMode.LOG_ONLY, true, true, ""));
            screenshotSettings.set(service, store.settings());
            screenshotRevision.set(service, field(service, "requestRevision"));
            for (int index = 0; index < phases.size(); index++) {
                var phase = phases.get(index);
                var save = new SaveSnapshot(phase, "fixed-request", "fixed-session", 1_000_000_000L, 30_000_000_000L,
                    30, Path.of("/disposable/actual-result"), "空き容量を確認してください。", List.of("device_log"),
                    device, app, List.of(), null, CaptureMode.LOG_ONLY, 2L, null, Path.of("/disposable/preserved.partial"));
                var auto = new AutoSaveSnapshot(io.github.shinma06.replaybuffer.core.AutoSaveState.WAITING,
                    "報告待ち", 7, 1, null, "自動保存の帰属は未確認です");
                var snapshot = new ReplaySnapshot(index + 1L, 1, true, states.get(index), settings, 1,
                    device, "current-session", 30_000_000_000L, index == 3, video, logs, logs, save,
                    index == 0 || index == 3, "固定した対象を確認してください。", null, 1_000_000_000L, 0, auto, save);
                view.set(capture, new ReplayCaptureView(snapshot, 0, null, null));
                render.invoke(panel);
                panel.setSize(360, 700);
                layout(panel); layout(panel);
                var failed = phase == io.github.shinma06.replaybuffer.core.SavePhase.FAILED;
                var complete = phase == io.github.shinma06.replaybuffer.core.SavePhase.COMPLETED;
                assertEquals(failed, ((JPanel) field(panel, "failureCard")).isVisible());
                assertEquals(failed, ((AbstractButton) field(panel, "retryButton")).isEnabled());
                assertEquals(failed, ((AbstractButton) field(panel, "retryElsewhere")).isEnabled());
                assertEquals(failed, ((AbstractButton) field(panel, "discardButton")).isEnabled());
                assertEquals(complete, ((AbstractButton) field(panel, "openButton")).isVisible());
                assertEquals(index == 0 || index == 3, ((AbstractButton) field(panel, "saveButton")).isEnabled());
                var output = (JTextArea) field(panel, "saveOutput");
                assertEquals(failed || complete, output.isVisible());
                if (failed) {
                    assertTrue(output.getText().contains("/disposable/preserved.partial"));
                    assertTrue(((JTextArea) field(panel, "failure")).getText().contains("空き容量"));
                    assertTrue(((JTextArea) field(panel, "frozenRequest")).getText().contains("固定対象: 1.000秒〜30.000秒"));
                    assertTrue(((JTextArea) field(panel, "frozenTarget")).getText().contains("com.fixture.visible"));
                }
                if (complete) {
                    assertTrue(output.getText().contains("/disposable/actual-result"));
                    assertFalse(output.getText().contains("configured"));
                    assertTrue(((JTextArea) field(panel, "saveState")).getText().contains("記録の不足・未確認: 端末ログ"));
                }
                assertFalse(((JTextArea) field(panel, "message")).isVisible(), "empty status must not add an unexplained row");
                assertTrue(((JTextArea) field(panel, "state")).getText().contains("ログ専用"));
                assertEquals("動画: 対象外（ログ専用）", ((JTextArea) field(panel, "video")).getText());
                assertTrue(((JTextArea) field(panel, "gapDetails")).getText().contains("時計対応を確認できません"));
                assertTrue(((JTextArea) field(panel, "gapDetails")).getText().contains("未確定〜未確定"));
                assertTrue(((JTextArea) field(panel, "autoSave")).getText().contains("自動保存の帰属は未確認"));
                assertTrue(((JTextArea) field(panel, "window")).getText().contains("保持時間: 30秒"));
                assertTrue(bounds(panel, "state").y < bounds(panel, "saveButton").y);
                assertTrue(bounds(panel, "saveButton").y < bounds(panel, "video").y);
                assertTrue(bounds(panel, "screenshotButton").y < bounds(panel, "sdk").y);
                assertTrue(bounds(panel, "saveState").y < bounds(panel, "video").y);
                if (failed) assertTrue(bounds(panel, "retryButton").y < bounds(panel, "frozenTarget").y);
                for (var name : List.of("state", "device", "application", "window", "saveReason", "screenshotReason", "saveOutput"))
                    assertFalse(((JComponent) field(panel, name)).getAccessibleContext().getAccessibleName().isBlank(), name);
                verifyFirstPaint(panel, "tool-window-" + phase.name().toLowerCase(java.util.Locale.ROOT));
            }
            view.set(capture, new ReplayCaptureView(null, 0, null, "一時領域のアクセス権を確認してください。"));
            render.invoke(panel);
            assertTrue(((JTextArea) field(panel, "state")).getText().contains("初期化できません"));
            assertTrue(((JTextArea) field(panel, "message")).isVisible());
            assertTrue(((JTextArea) field(panel, "message")).getText().contains("一時領域のアクセス権"));
            assertFalse(((AbstractButton) field(panel, "saveButton")).isEnabled());
        } finally {
            view.set(capture, originalView);
            resolving.setBoolean(service, originalResolving);
            store.loadState(originalSettings);
            screenshotSettings.set(service, originalScreenshotSettings);
            screenshotRevision.setLong(service, originalScreenshotRevision);
            render.invoke(panel);
        }
    }

    private static java.awt.Rectangle bounds(JPanel panel, String name) throws ReflectiveOperationException {
        var component = (JComponent) field(panel, name);
        return SwingUtilities.convertRectangle(component.getParent(), component.getBounds(), panel);
    }

    private static void verifyLargeFontPresentation(JPanel panel) throws Exception {
        var previous = javax.swing.UIManager.getLookAndFeel();
        var fonts = new java.util.IdentityHashMap<Component, java.awt.Font>();
        try {
            javax.swing.UIManager.setLookAndFeel(new javax.swing.plaf.nimbus.NimbusLookAndFeel());
            SwingUtilities.updateComponentTreeUI(panel);
            collectFonts(panel, fonts);
            fonts.forEach((component, font) -> {
                component.setFont(font.deriveFont(font.getSize2D() * 1.5f));
                assertEquals(font.getSize2D() * 1.5f, component.getFont().getSize2D(), 0.01f);
            });
            for (int width : new int[]{280, 900, 280}) {
                panel.setSize(width, 700);
                layout(panel); layout(panel);
                var outer = (JScrollPane) panel.getComponent(0);
                var form = (JComponent) outer.getViewport().getView();
                assertFalse(outer.getHorizontalScrollBar().isVisible());
                verifyFirstPaint(panel, "tool-window-" + width + "-large-font");
                for (var name : List.of("enabled", "settingsButton", "saveButton", "screenshotButton", "retryButton", "retryElsewhere", "discardButton")) {
                    var button = (AbstractButton) field(panel, name);
                    var area = SwingUtilities.convertRectangle(button.getParent(), button.getBounds(), form);
                    assertTrue(area.width >= button.getPreferredSize().width, "full button label at enlarged font: " + name);
                    assertTrue(button.getY() >= 0 && button.getY() + button.getHeight() <= button.getParent().getHeight(),
                        name + " must not be clipped by its wrapping row: button=" + button.getBounds() + ", row=" +
                            button.getParent().getSize() + ", preferred=" + button.getParent().getPreferredSize());
                    assertTrue(area.x >= 0 && area.getMaxX() <= form.getWidth(), name + " must fit enlarged font at " + width);
                    form.scrollRectToVisible(area);
                    assertTrue(outer.getViewport().getViewRect().contains(area), name + " must remain reachable");
                    assertNotNull(button.getInputMap(JComponent.WHEN_FOCUSED).get(KeyStroke.getKeyStroke("SPACE")));
                }
            }
        } finally {
            fonts.forEach(Component::setFont);
            javax.swing.UIManager.setLookAndFeel(previous);
            SwingUtilities.updateComponentTreeUI(panel);
        }
    }

    private static void collectFonts(Component component, java.util.Map<Component, java.awt.Font> originals) {
        if (component.getFont() != null) originals.put(component, component.getFont());
        if (component instanceof Container parent) for (var child : parent.getComponents()) collectFonts(child, originals);
    }

    private static void verifyFirstPaint(JPanel panel, String name) throws Exception {
        var first = writePreview(panel, name + "-first-paint");
        verifyWrappedText(panel);
        var measured = writePreview(panel, name + "-after-measurement");
        org.junit.jupiter.api.Assertions.assertArrayEquals(
            ((java.awt.image.DataBufferInt) first.getRaster().getDataBuffer()).getData(),
            ((java.awt.image.DataBufferInt) measured.getRaster().getDataBuffer()).getData(),
            "modelToView measurement must not repair the first paint: " + name);
    }

    private static void verifyWrappedText(Container parent) throws javax.swing.text.BadLocationException {
        for (var component : parent.getComponents()) {
            if (!component.isVisible()) continue;
            if (component instanceof JTextArea text && !text.isFocusable()) {
                for (int offset = 0; offset < text.getText().length(); offset++) {
                    var character = text.modelToView2D(offset);
                    assertTrue(character != null && character.getX() >= 0 && character.getMaxX() <= text.getWidth()
                        && character.getY() >= 0 && character.getMaxY() <= text.getHeight(),
                        "wrapped text must fit: " + text.getAccessibleContext().getAccessibleName() + ", offset=" + offset +
                            ", character=" + character + ", size=" + text.getSize());
                }
            }
            if (component instanceof Container container) verifyWrappedText(container);
        }
    }

    private static java.awt.image.BufferedImage writePreview(JPanel panel, String name) throws java.io.IOException {
        var outer = (JScrollPane) panel.getComponent(0);
        outer.getViewport().setViewPosition(new Point());
        var image = new java.awt.image.BufferedImage(panel.getWidth(), panel.getHeight(), java.awt.image.BufferedImage.TYPE_INT_RGB);
        var graphics = image.createGraphics();
        try { panel.printAll(graphics); } finally { graphics.dispose(); }
        var path = Path.of("build/reports/tests/ui", name + ".png");
        Files.createDirectories(path.getParent());
        javax.imageio.ImageIO.write(image, "png", path.toFile());
        return image;
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
            io.github.shinma06.replaybuffer.settings.AppSelectionMode.AUTOMATIC, "", CaptureMode.VIDEO_AND_LOGS, false, true, ""));
        var revision = service.getClass().getDeclaredField("screenshotRevision");
        revision.setAccessible(true);
        revision.set(service, field(service, "requestRevision"));
        var render = panel.getClass().getDeclaredMethod("render");
        render.setAccessible(true);
        render.invoke(panel);
        assertTrue(button.isEnabled(), "screenshot is available with replay OFF and capture initialization pending");
        var serial = service.getClass().getDeclaredMethod("screenshotDeviceSerial", ReplaySnapshot.class);
        serial.setAccessible(true);
        assertNull(serial.invoke(service, (Object) null), "OFF uses shared device resolution without starting capture");
        store.setEnabled(true);
        render.invoke(panel);
        assertFalse(button.isEnabled(), "pending startup must not independently select a different device");
        var capture = field(service, "capture");
        var view = capture.getClass().getDeclaredField("view");
        view.setAccessible(true);
        var activeSettings = new ReplaySettings(Path.of("/disposable/adb"), Path.of("/disposable"),
            180, new ApplicationTarget(), CaptureMode.VIDEO_AND_LOGS, false);
        for (boolean connected : new boolean[]{true, false}) {
            var snapshot = new ReplaySnapshot(1, 1, true, CaptureState.CAPTURING, activeSettings, 1,
                new ReplayDevice("emulator-5554", "Recording", DeviceKind.EMULATOR, connected),
                "screenshot-test", 1L, false,
                new StreamSnapshot(), new StreamSnapshot(), new StreamSnapshot(), new SaveSnapshot(),
                false, null, null, 0L, 0, new AutoSaveSnapshot(), null);
            view.set(capture, new ReplayCaptureView(snapshot, 0, null, null));
            render.invoke(panel);
            assertTrue(button.isEnabled());
            assertEquals("emulator-5554", serial.invoke(service, snapshot), "disconnection must retain the recording serial");
            store.setEnabled(false);
            assertEquals("emulator-5554", serial.invoke(service, snapshot), "pending OFF must not lose the actual capture target");
            store.setEnabled(true);
        }
        environment.set(service, new io.github.shinma06.replaybuffer.ide.AndroidReplayEnvironment(
            Path.of("/different-sdk/adb"), null,
            new io.github.shinma06.replaybuffer.ide.ApplicationSelection("sample.app", "sample", null)));
        render.invoke(panel);
        assertFalse(button.isEnabled(), "pending SDK change cannot resolve the recording serial on another server");
        environment.set(service, new io.github.shinma06.replaybuffer.ide.AndroidReplayEnvironment(
            Path.of("/disposable/adb"), null,
            new io.github.shinma06.replaybuffer.ide.ApplicationSelection("sample.app", "sample", null)));
        view.set(capture, new ReplayCaptureView(null, 0, null, null));
        store.setEnabled(false);
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
