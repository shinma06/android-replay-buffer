package io.github.shinma06.replaybuffer.fixture;

import android.app.Activity;
import android.app.Application;
import android.content.Intent;
import android.graphics.Insets;
import android.os.Build;
import android.os.Bundle;
import android.os.Process;
import android.util.Log;
import android.view.DisplayCutout;
import android.view.View;
import android.view.WindowInsets;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import java.util.UUID;

/** Explicitly launched disposable inputs; the normal marker screen is unchanged. */
public final class CrashTestActivity extends Activity {
    private static final String TAG = "REPLAY_CRASH_QA";

    @Override
    public void onCreate(Bundle state) {
        super.onCreate(state);
        String identity = identity();
        LinearLayout layout = content(this, "クラッシュ自動保存の試験", identity);
        TextView status = new TextView(this);
        status.setTag("crash_test_status");
        status.setText("待機中。各入力はボタンを押したときだけ実行します。");
        status.setAccessibilityLiveRegion(View.ACCESSIBILITY_LIVE_REGION_POLITE);
        layout.addView(status);
        button(layout, "ordinary_error", "通常のERRORログ", view -> {
            Log.e(TAG, identity + " action=ordinary_error");
            status.setText("ERRORログを出しました。アプリは継続しています。");
        });
        button(layout, "caught", "catchする例外", view -> {
            try {
                throw new IllegalStateException(identity + " action=caught");
            } catch (IllegalStateException error) {
                Log.e(TAG, "Caught fixture exception", error);
                status.setText("例外をcatchしました。アプリは継続しています。");
            }
        });
        button(layout, "custom_handler", "このThreadだけのhandlerで例外を処理", view -> {
            view.setEnabled(false);
            Thread worker = new Thread(() -> {
                throw new IllegalStateException(identity + " action=custom_handler");
            }, "replay-crash-guard");
            worker.setUncaughtExceptionHandler((thread, error) -> {
                Log.e(TAG, "Thread-local fixture handler", error);
                runOnUiThread(() -> {
                    if (!isFinishing() && !isDestroyed()) {
                        status.setText("Threadの例外を処理しました。アプリは継続しています。");
                        view.setEnabled(true);
                    }
                });
            });
            worker.start();
        });
        button(layout, "normal_finish", "この画面を通常終了", view -> {
            Log.i(TAG, identity + " action=normal_finish");
            finish();
        });
        button(layout, "uncaught_main", "未処理例外でこのアプリを終了", view -> crash(identity, "uncaught_main"));
        button(layout, "open_subprocess", ":crash の試験画面を開く", view ->
                startActivity(new Intent(this, Subprocess.class)));
        Log.i(TAG, identity + " action=panel_open");
    }

    private static String identity() {
        return "package=" + BuildConfig.APPLICATION_ID + " source=" + BuildConfig.FIXTURE_SOURCE
                + " process=" + Application.getProcessName() + " pid=" + Process.myPid()
                + " uid=" + Process.myUid() + " run=" + UUID.randomUUID();
    }

    private static void crash(String identity, String action) {
        Log.i(TAG, identity + " action=" + action);
        throw new IllegalStateException(identity + " action=" + action);
    }

    private static void button(LinearLayout layout, String action, String label, View.OnClickListener listener) {
        Button button = new Button(layout.getContext());
        button.setTag(action);
        button.setText(label);
        button.setOnClickListener(listener);
        layout.addView(button);
    }

    private static LinearLayout content(Activity activity, String title, String identity) {
        ScrollView scroll = new ScrollView(activity);
        LinearLayout layout = new LinearLayout(activity);
        layout.setOrientation(LinearLayout.VERTICAL);
        int padding = Math.round(16 * activity.getResources().getDisplayMetrics().density);
        scroll.setPadding(padding, padding, padding, padding);
        scroll.setOnApplyWindowInsetsListener((view, insets) -> {
            Insets safe;
            if (Build.VERSION.SDK_INT >= 30) {
                safe = insets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
            } else {
                safe = insets.getSystemWindowInsets();
                DisplayCutout cutout = insets.getDisplayCutout();
                if (cutout != null) {
                    safe = Insets.of(Math.max(safe.left, cutout.getSafeInsetLeft()),
                            Math.max(safe.top, cutout.getSafeInsetTop()),
                            Math.max(safe.right, cutout.getSafeInsetRight()),
                            Math.max(safe.bottom, cutout.getSafeInsetBottom()));
                }
            }
            view.setPadding(padding + safe.left, padding + safe.top, padding + safe.right, padding + safe.bottom);
            return insets;
        });
        TextView heading = new TextView(activity);
        heading.setText(title + "\n" + identity.replace(" ", "\n"));
        heading.setTextIsSelectable(true);
        layout.addView(heading);
        scroll.addView(layout);
        activity.setContentView(scroll);
        scroll.requestApplyInsets();
        return layout;
    }

    /** Private app process: opening this screen does not itself crash. */
    public static final class Subprocess extends Activity {
        @Override
        public void onCreate(Bundle state) {
            super.onCreate(state);
            String identity = identity();
            LinearLayout layout = content(this, "子process :crash の試験", identity);
            button(layout, "uncaught_subprocess", "未処理例外でこの子processを終了", view -> crash(identity, "uncaught_subprocess"));
            Log.i(TAG, identity + " action=subprocess_open");
        }
    }
}
