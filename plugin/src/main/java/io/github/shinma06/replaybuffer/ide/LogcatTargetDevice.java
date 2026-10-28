package io.github.shinma06.replaybuffer.ide;

import com.android.tools.idea.logcat.LogcatMainPanel;
import com.android.tools.idea.logcat.LogcatToolWindowFactory;
import com.android.tools.idea.logcat.devices.Device;
import com.android.tools.idea.logcat.devices.DeviceComboBox.DeviceComboItem.DeviceItem;
import com.android.tools.idea.logcat.devices.DeviceComboBox.DeviceComboItem.FileItem;
import com.intellij.openapi.util.Disposer;
import com.intellij.openapi.wm.ToolWindow;
import com.intellij.ui.content.Content;
import com.intellij.ui.content.ContentManager;
import java.awt.event.ActionEvent;
import java.awt.event.ActionListener;
import java.util.Objects;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CancellationException;
import java.util.function.BooleanSupplier;
import java.util.function.Supplier;
import javax.swing.JComboBox;
import javax.swing.SwingUtilities;
import javax.swing.Timer;

/** EDT-only Studio ABI boundary; the caller gates the exact IDE build before loading this class.
 * Null completion permits showing, not rendered Logcat acceptance. Cancel on content disposal.
 */
public final class LogcatTargetDevice {
    private LogcatTargetDevice() {}

    public static CompletableFuture<String> prepare(ToolWindow window, String serial, BooleanSupplier active, long deadlineNs) {
        if (!SwingUtilities.isEventDispatchThread()) throw new IllegalStateException("Logcat selection requires EDT");
        if (System.nanoTime() >= deadlineNs) return CompletableFuture.completedFuture("標準Logcatの準備が期限内に完了しませんでした。再試行してください。");
        if (window.isDisposed() || !window.isAvailable()) return CompletableFuture.completedFuture("IDE標準Logcatを利用できません。");
        var manager = window.getContentManager(); // Native lazy factory initializes cold/restored content here.
        var selected = manager.getSelectedContent();
        if (selected == null || Disposer.isDisposed(selected)) return CompletableFuture.completedFuture("標準Logcatで端末用のタブをIDE側から開いて再試行してください。");

        var pane = singlePane(manager, selected);
        if (pane == null) return CompletableFuture.completedFuture("標準Logcatで分割していない端末用のタブを選び直して再試行してください。");
        return awaitDevice(pane.getHeaderPanel().getDeviceComboBox(), serial, pane::getConnectedDevice,
            () -> active.getAsBoolean() && !window.isDisposed() && window.isAvailable() && !window.isVisible()
                && !Disposer.isDisposed(selected) && !Disposer.isDisposed(pane)
                && manager.getSelectedContent() == selected && singlePane(manager, selected) == pane,
            deadlineNs);
    }

    private static LogcatMainPanel singlePane(ContentManager manager, Content selected) {
        // shortcut: the supported Studio's internal JVM ABI; recheck this boundary when updating Studio.
        LogcatMainPanel pane = null;
        for (var presenter : LogcatToolWindowFactory.Companion.getLogcatPresenters$intellij_android_logcat()) {
            if (presenter instanceof LogcatMainPanel candidate && !Disposer.isDisposed(candidate)
                && manager.getContent(candidate) == selected) {
                if (pane != null) return null;
                pane = candidate;
            }
        }
        return pane;
    }

    /** One bounded EDT wait for the native model/connection; deadline is monotonic nanoseconds. */
    static CompletableFuture<String> awaitDevice(JComboBox<?> box, String serial, Supplier<Device> connected,
                                                BooleanSupplier active, long deadlineNs) {
        var result = new CompletableFuture<String>();
        var initiallyEmpty = box.getItemCount() == 0;
        var listener = new ActionListener() {
            private Object expected = selectionIdentity(box.getSelectedItem());
            private boolean selected;

            @Override public void actionPerformed(ActionEvent event) {
                if (result.isDone()) return;
                if (!SwingUtilities.isEventDispatchThread()) {
                    SwingUtilities.invokeLater(() -> actionPerformed(null));
                    return;
                }
                try {
                    if (!active.getAsBoolean()) {
                        result.complete("Logcatの対象・タブ・表示状態が変わったため、開く操作を中止しました。");
                    } else if (serial == null || serial.isBlank()) {
                        result.complete("Replay Bufferの対象端末がありません。");
                    } else if (box.getSelectedItem() instanceof FileItem) {
                        result.complete("保存ログを表示中です。標準Logcatで端末用のタブを選んで再試行してください。");
                    } else if (System.nanoTime() >= deadlineNs) {
                        result.complete("標準Logcatの準備が完了しませんでした。接続と端末一覧を確認して再試行してください。");
                    } else {
                        DeviceItem target = null;
                        for (int i = 0; i < box.getItemCount(); i++) {
                            if (box.getItemAt(i) instanceof DeviceItem item && serial.equals(item.getDevice().getSerialNumber())) {
                                if (!item.getDevice().isOnline()) {
                                    if (selected) {
                                        result.complete("標準Logcatの対象端末が切断されました。再接続後に改めて開いてください。");
                                        return;
                                    }
                                    // Restored identities arrive offline before live discovery.
                                    continue;
                                }
                                if (target != null) {
                                    result.complete("標準Logcatの対象端末を一意に確認できません。");
                                    return;
                                }
                                target = item;
                            }
                        }
                        if (selected && target == null) {
                            result.complete("標準Logcatの対象端末が一覧から消えたため、開く操作を中止しました。");
                            return;
                        }
                        var current = box.getSelectedItem();
                        // The first item is selected by the native cold model, before it can offer a user choice.
                        if (!selected && initiallyEmpty && expected == null && current instanceof DeviceItem) {
                            expected = selectionIdentity(current);
                        }
                        if (!Objects.equals(expected, selectionIdentity(current))) {
                            result.complete("標準Logcatの端末選択が変更されたため、開く操作を中止しました。");
                        } else if (target != null) {
                            if (!selected) {
                                selected = true;
                                expected = selectionIdentity(target);
                                if (current != target) box.setSelectedItem(target);
                            }
                            if (!(box.getSelectedItem() instanceof DeviceItem item) || !matches(item.getDevice(), serial)) {
                                result.complete("標準Logcatの対象端末が変わりました。再試行してください。");
                            } else {
                                var connection = connected.get();
                                if (connection != null && serial.equals(connection.getSerialNumber()) && !connection.isOnline()) {
                                    result.complete("標準Logcatの対象端末が切断されました。再接続後に改めて開いてください。");
                                } else if (matches(connection, serial)) {
                                    result.complete(null);
                                }
                            }
                        }
                    }
                } catch (CancellationException cancelled) {
                    result.cancel(false);
                } catch (LinkageError | RuntimeException error) {
                    result.completeExceptionally(error);
                }
            }
        };
        var timer = new Timer(100, listener);
        box.addActionListener(listener);
        result.whenComplete((reason, error) -> {
            Runnable stop = () -> { timer.stop(); box.removeActionListener(listener); };
            if (SwingUtilities.isEventDispatchThread()) stop.run(); else SwingUtilities.invokeLater(stop);
        });
        listener.actionPerformed(null);
        if (!result.isDone()) timer.start();
        return result;
    }

    private static Object selectionIdentity(Object item) {
        return item instanceof DeviceItem device ? device.getDevice().getDeviceId() : item;
    }

    private static boolean matches(Device device, String serial) {
        return device != null && device.isOnline() && serial.equals(device.getSerialNumber());
    }
}
