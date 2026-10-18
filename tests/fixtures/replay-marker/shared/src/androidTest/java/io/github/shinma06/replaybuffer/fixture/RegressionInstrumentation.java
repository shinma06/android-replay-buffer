package io.github.shinma06.replaybuffer.fixture;

import android.app.Activity;
import android.app.Instrumentation;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.graphics.Insets;
import android.os.Build;
import android.os.Bundle;
import android.os.ParcelFileDescriptor;
import android.os.Process;
import android.os.SystemClock;
import android.view.DisplayCutout;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.widget.Button;
import android.widget.TextView;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.util.HashMap;
import java.util.Map;
import org.json.JSONObject;

/** Native regression runner; only runs after the QA lease/install assignment. */
public final class RegressionInstrumentation extends Instrumentation {
    private Activity activity;
    private ViewGroup layout;
    private View marker;
    private Button emit;
    private String viewId;

    @Override
    public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        start();
    }

    @Override
    public void onStart() {
        Bundle result = new Bundle();
        int code = Activity.RESULT_OK;
        try {
            open();
            awaitPhases(event(), false);
            runOnMainSync(() -> emit.performClick());
            awaitPhases(event(), true); // No second tap/invalidation to trigger commit.
            runOnMainSync(() -> { emit.performClick(); emit.performClick(); });
            Map<String, Long> coalesced = awaitPhases(event(), true);
            check(!phases(event() - 1).containsKey("DRAW"), "coalesced request unexpectedly drawn");
            check(coalesced.get("DRAW") <= coalesced.get("FRAME_COMMIT"), "commit preceded draw");
            runOnMainSync(() -> {
                int padding = Math.round(16 * activity.getResources().getDisplayMetrics().density);
                WindowInsets.Builder builder = new WindowInsets.Builder();
                Insets bars = Insets.of(11, 29, 17, 83);
                if (Build.VERSION.SDK_INT >= 30) {
                    builder.setInsets(WindowInsets.Type.systemBars(), bars);
                } else {
                    builder.setSystemWindowInsets(bars);
                }
                builder.setDisplayCutout(new DisplayCutout(Insets.of(41, 47, 23, 97), null, null, null, null));
                WindowInsets insets = builder.build();
                for (int i = 0; i < 2; i++) {
                    layout.dispatchApplyWindowInsets(insets);
                    check(layout.getPaddingLeft() == padding + 41, "left cutout inset");
                    check(layout.getPaddingTop() == padding + 47, "top cutout inset");
                    check(layout.getPaddingRight() == padding + 23, "right cutout inset");
                    check(layout.getPaddingBottom() == padding + 97, "bottom inset / repeated dispatch");
                }
                layout.requestApplyInsets();
            });
            // Finish with an update pending; the next Activity must get its own callback.
            runOnMainSync(() -> { emit.performClick(); activity.finish(); });
            long deadline = SystemClock.uptimeMillis() + 5000;
            while (!activity.isDestroyed() && SystemClock.uptimeMillis() < deadline) SystemClock.sleep(50);
            check(activity.isDestroyed(), "Activity did not dispose");
            open();
            awaitPhases(event(), false);
            runOnMainSync(() -> emit.performClick());
            awaitPhases(event(), true);
            verifyCrashGuards();
            open();
            runOnMainSync(() -> {
                check(layout.getChildCount() == 3, "normal marker layout changed");
                emit.performClick();
            });
            awaitPhases(event(), true);
            result.putString("stream", "PASS: idle commit, coalescing, dispose/reopen, bars/cutout insets, crash-panel nonfatal guards\n");

        } catch (Exception | AssertionError error) {
            result.putString("stream", "FAIL: " + error.getMessage() + "\n");
            code = Activity.RESULT_CANCELED;
        } finally {
            if (activity != null) runOnMainSync(() -> activity.finish());
        }
        finish(code, result);
    }

    private void open() {
        Intent intent = new Intent().setClassName(getTargetContext().getPackageName(), MarkerActivity.class.getName());
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        activity = startActivitySync(intent);
        runOnMainSync(() -> {
            layout = (ViewGroup) ((ViewGroup) activity.findViewById(android.R.id.content)).getChildAt(0);
            marker = layout.getChildAt(1);
            emit = (Button) layout.getChildAt(2);
            String identity = ((TextView) layout.getChildAt(0)).getText().toString();
            viewId = identity.substring(identity.indexOf("\nview=") + 6);
        });
    }

    private long event() {
        long[] value = new long[1];
        runOnMainSync(() -> value[0] = Long.parseLong(marker.getContentDescription().toString().split(" ")[1]));
        return value[0];
    }

    private void verifyCrashGuards() throws Exception {
        runOnMainSync(() -> activity.finish());
        Intent intent = new Intent().setClassName(getTargetContext().getPackageName(), CrashTestActivity.class.getName());
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        activity = startActivitySync(intent);
        int pid = Process.myPid();
        Thread.UncaughtExceptionHandler original = Thread.getDefaultUncaughtExceptionHandler();
        Button[] custom = new Button[1];
        TextView[] status = new TextView[1];
        runOnMainSync(() -> {
            View content = activity.findViewById(android.R.id.content);
            status[0] = content.findViewWithTag("crash_test_status");
            for (String action : new String[] {"ordinary_error", "caught", "custom_handler", "normal_finish", "uncaught_main", "open_subprocess"}) {
                Button button = content.findViewWithTag(action);
                check(button != null && button.isEnabled() && button.getText().length() > 0, "missing labeled action: " + action);
            }
            ((Button) content.findViewWithTag("ordinary_error")).performClick();
            check(status[0].getText().toString().contains("ERRORログを出しました"), "ordinary error action did not return");
            ((Button) content.findViewWithTag("caught")).performClick();
            check(status[0].getText().toString().contains("例外をcatchしました"), "caught action did not return");
            custom[0] = content.findViewWithTag("custom_handler");
            custom[0].performClick();
        });
        long deadline = SystemClock.uptimeMillis() + 5000;
        boolean[] handled = new boolean[1];
        do {
            runOnMainSync(() -> handled[0] = custom[0].isEnabled()
                    && status[0].getText().toString().contains("Threadの例外を処理しました"));
            if (handled[0]) break;
            SystemClock.sleep(50);
        } while (SystemClock.uptimeMillis() < deadline);
        check(handled[0], "thread-local handler did not finish");
        check(Process.myPid() == pid, "nonfatal guards changed the process");
        check(Thread.getDefaultUncaughtExceptionHandler() == original, "process-wide handler changed");
        ActivityInfo child = getTargetContext().getPackageManager().getActivityInfo(
                new ComponentName(getTargetContext(), CrashTestActivity.Subprocess.class), 0);
        check(!child.exported && child.processName.equals(getTargetContext().getPackageName() + ":crash"),
                "subprocess must be private and use :crash");
        runOnMainSync(() -> ((Button) activity.findViewById(android.R.id.content).findViewWithTag("normal_finish")).performClick());
        deadline = SystemClock.uptimeMillis() + 5000;
        while (!activity.isDestroyed() && SystemClock.uptimeMillis() < deadline) SystemClock.sleep(50);
        check(activity.isDestroyed(), "normal finish action did not finish the panel");
    }

    private Map<String, Long> awaitPhases(long event, boolean request) throws Exception {
        long deadline = SystemClock.uptimeMillis() + 5000;
        do {
            Map<String, Long> found = phases(event);
            if (found.containsKey("DRAW") && found.containsKey("FRAME_COMMIT")
                    && (!request || found.containsKey("REQUEST"))) {
                check(found.get("DRAW") <= found.get("FRAME_COMMIT"), "wrong frame commit order");
                return found;
            }
            SystemClock.sleep(50);
        } while (SystemClock.uptimeMillis() < deadline);
        throw new AssertionError("Missing phases for event " + event + " without another frame");
    }

    private Map<String, Long> phases(long event) throws Exception {
        Map<String, Long> found = new HashMap<>();
        try (ParcelFileDescriptor pipe = getUiAutomation().executeShellCommand("logcat -d -v raw -s REPLAY_QA:I '*:S'");
                BufferedReader reader = new BufferedReader(new InputStreamReader(new ParcelFileDescriptor.AutoCloseInputStream(pipe)))) {
            String line;
            while ((line = reader.readLine()) != null) {
                if (!line.startsWith("{")) continue;
                JSONObject record = new JSONObject(line);
                if (record.optString("package").equals(getTargetContext().getPackageName())
                        && record.optString("view").equals(viewId) && record.optLong("event", -1) == event) {
                    String phase = record.getString("phase");
                    check(!found.containsKey(phase), "duplicate phase " + phase);
                    found.put(phase, record.getLong("elapsed_before_ns"));
                }
            }
        }
        return found;
    }

    private static void check(boolean condition, String reason) {
        if (!condition) throw new AssertionError(reason);
    }
}
