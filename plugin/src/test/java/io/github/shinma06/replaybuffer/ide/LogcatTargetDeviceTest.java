package io.github.shinma06.replaybuffer.ide;

import com.android.sdklib.AndroidVersion;
import com.android.sdklib.deviceprovisioner.DeviceId;
import com.android.sdklib.deviceprovisioner.DeviceType;
import com.android.tools.idea.logcat.devices.Device;
import com.android.tools.idea.logcat.devices.DeviceComboBox.DeviceComboItem;
import com.android.tools.idea.logcat.devices.DeviceComboBox.DeviceComboItem.DeviceItem;
import com.android.tools.idea.logcat.devices.DeviceComboBox.DeviceComboItem.FileItem;
import java.nio.file.Path;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import javax.swing.JComboBox;
import javax.swing.SwingUtilities;
import org.junit.jupiter.api.Test;
import static org.junit.jupiter.api.Assertions.*;

class LogcatTargetDeviceTest {
    private static DeviceItem device(String serial, boolean online) {
        return new DeviceItem(Device.Companion.createPhysical(new DeviceId("fixture", false, serial), serial, online,
            "17", new AndroidVersion(37), "same", "same", DeviceType.HANDHELD));
    }

    @Test
    void selectsExactSerialAndWaitsForTheNativeStreamFromOneRequest() throws Exception {
        var request = new AtomicReference<CompletableFuture<String>>();
        var target = device("A", true);
        var connected = new AtomicReference<>(device("B", true).getDevice());
        try {
            SwingUtilities.invokeAndWait(() -> {
                var box = new JComboBox<DeviceComboItem>(new DeviceComboItem[]{new DeviceItem(connected.get()), target});
                request.set(LogcatTargetDevice.awaitDevice(box, "A", connected::get, () -> true,
                    System.nanoTime() + TimeUnit.SECONDS.toNanos(3)));
                assertSame(target, box.getSelectedItem());
                assertFalse(request.get().isDone()); // Model selection is not native stream completion.
                box.addActionListener(event -> fail("Waiting must not repeatedly select/restart the target"));
                connected.set(target.getDevice());
            });
            assertNull(request.get().get(2, TimeUnit.SECONDS));
        } finally {
            if (request.get() != null) request.get().cancel(false);
        }
    }

    @Test
    void nativeModelCanArriveLaterWithoutAnotherClick() throws Exception {
        var request = new AtomicReference<CompletableFuture<String>>();
        var target = device("A", true);
        try {
            SwingUtilities.invokeAndWait(() -> {
                var box = new JComboBox<DeviceComboItem>();
                request.set(LogcatTargetDevice.awaitDevice(box, "A", target::getDevice, () -> true,
                    System.nanoTime() + TimeUnit.SECONDS.toNanos(3)));
                assertFalse(request.get().isDone());
                box.addItem(device("B", true)); // Native cold initialization may offer another device first.
                box.addItem(target);
            });
            assertNull(request.get().get(2, TimeUnit.SECONDS));
        } finally {
            if (request.get() != null) request.get().cancel(false);
        }
    }

    @Test
    void manualSelectionDuringSwitchIsPreservedAndCancelsTheRequest() throws Exception {
        var request = new AtomicReference<CompletableFuture<String>>();
        var box = new AtomicReference<JComboBox<DeviceComboItem>>();
        var other = device("B", true);
        try {
            SwingUtilities.invokeAndWait(() -> {
                box.set(new JComboBox<>(new DeviceComboItem[]{other, device("A", true)}));
                request.set(LogcatTargetDevice.awaitDevice(box.get(), "A", other::getDevice, () -> true,
                    System.nanoTime() + TimeUnit.SECONDS.toNanos(3)));
                box.get().setSelectedItem(other);
            });
            assertTrue(request.get().get(2, TimeUnit.SECONDS).contains("変更"));
            SwingUtilities.invokeAndWait(() -> assertSame(other, box.get().getSelectedItem()));
        } finally {
            if (request.get() != null) request.get().cancel(false);
        }
    }

    @Test
    void unavailableAmbiguousSavedFileAndExpiredOrCancelledRequestsNeverFallBack() throws Exception {
        SwingUtilities.invokeAndWait(() -> {
            var other = device("B", true);
            var box = new JComboBox<DeviceComboItem>(new DeviceComboItem[]{other, device("A", false)});
            var pending = LogcatTargetDevice.awaitDevice(box, "A", other::getDevice, () -> true, Long.MAX_VALUE);
            assertFalse(pending.isDone());
            assertSame(other, box.getSelectedItem());
            pending.cancel(false);
            assertNotNull(LogcatTargetDevice.awaitDevice(box, "A", other::getDevice, () -> true, 0).join());
            assertNotNull(LogcatTargetDevice.awaitDevice(box, "A", other::getDevice, () -> false, Long.MAX_VALUE).join());
            var file = new FileItem(Path.of("fixture.logcat"));
            box.addItem(file);
            box.setSelectedItem(file);
            assertTrue(LogcatTargetDevice.awaitDevice(box, "A", other::getDevice, () -> true, Long.MAX_VALUE).join().contains("保存ログ"));
            assertSame(file, box.getSelectedItem());
            box.setSelectedItem(other);
            box.addItem(device("A", true));
            box.addItem(device("A", true));
            assertTrue(LogcatTargetDevice.awaitDevice(box, "A", other::getDevice, () -> true, Long.MAX_VALUE).join().contains("一意"));
            assertSame(other, box.getSelectedItem());
        });
    }

}
