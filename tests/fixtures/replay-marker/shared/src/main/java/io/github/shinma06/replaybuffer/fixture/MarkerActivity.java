package io.github.shinma06.replaybuffer.fixture;

import android.app.Activity;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Insets;
import android.graphics.Paint;
import android.os.Build;
import android.os.Bundle;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;
import android.view.DisplayCutout;
import android.view.View;
import android.view.ViewTreeObserver;
import android.view.WindowInsets;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;
import java.util.UUID;
import org.json.JSONException;
import org.json.JSONObject;

/** Disposable marker app: no network, storage, background work, or recording. */
public final class MarkerActivity extends Activity {
    private static final String RUN = UUID.randomUUID().toString();
    private static long sequence;
    private final String viewId = UUID.randomUUID().toString();
    private MarkerView marker;
    private Runnable commitCallback;
    private ViewTreeObserver.OnPreDrawListener preDrawListener;

    @Override
    public void onCreate(Bundle state) {
        super.onCreate(state);
        LinearLayout layout = new LinearLayout(this);
        layout.setOrientation(LinearLayout.VERTICAL);
        int padding = Math.round(16 * getResources().getDisplayMetrics().density);
        layout.setPadding(padding, padding, padding, padding);
        layout.setOnApplyWindowInsetsListener((view, insets) -> {
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
            view.setPadding(padding + safe.left, padding + safe.top,
                    padding + safe.right, padding + safe.bottom);
            return insets;
        });
        TextView identity = new TextView(this);
        identity.setText("Replay QA\n" + BuildConfig.APPLICATION_ID + "\n"
                + BuildConfig.FIXTURE_SOURCE + "\nrun=" + RUN + "\nview=" + viewId);
        layout.addView(identity);
        marker = new MarkerView();
        preDrawListener = () -> {
            if (marker.drawn != marker.event) {
                unregisterCommit();
                long event = marker.event;
                if (marker.isHardwareAccelerated()) {
                    String committedView = viewId;
                    commitCallback = () -> stamp("FRAME_COMMIT", event, committedView);
                    marker.getViewTreeObserver().registerFrameCommitCallback(commitCallback);
                } else {
                    stamp("NO_HARDWARE_COMMIT", event, viewId);
                }
            }
            return true;
        };
        marker.getViewTreeObserver().addOnPreDrawListener(preDrawListener);
        layout.addView(marker, new LinearLayout.LayoutParams(-1, 0, 1));
        Button emit = new Button(this);
        emit.setText("イベント番号を更新");
        emit.setOnClickListener(view -> {
            unregisterCommit();
            long event = ++sequence;
            stamp("REQUEST", event, viewId);
            marker.event = event;
            marker.setContentDescription("イベント " + event);
            marker.invalidate();
        });
        layout.addView(emit);
        setContentView(layout);
        layout.requestApplyInsets();
        stamp("CREATE", sequence, viewId);
    }

    private static void stamp(String phase, long event, String view) {
        long before = SystemClock.elapsedRealtimeNanos();
        long wall = System.currentTimeMillis();
        long after = SystemClock.elapsedRealtimeNanos();
        try {
            JSONObject message = new JSONObject();
            message.put("schema", 1);
            message.put("package", BuildConfig.APPLICATION_ID);
            message.put("source", BuildConfig.FIXTURE_SOURCE);
            message.put("run", RUN);
            message.put("view", view);
            message.put("pid", Process.myPid());
            message.put("event", event);
            message.put("phase", phase);
            message.put("elapsed_before_ns", before);
            message.put("wall_ms", wall);
            message.put("elapsed_after_ns", after);
            Log.i("REPLAY_QA", message.toString());
        } catch (JSONException error) {
            throw new IllegalStateException("Fixture marker encoding failed", error);
        }
    }

    private void unregisterCommit() {
        if (commitCallback != null && marker.getViewTreeObserver().isAlive()) {
            marker.getViewTreeObserver().unregisterFrameCommitCallback(commitCallback);
        }
        commitCallback = null;
    }

    @Override
    protected void onDestroy() {
        unregisterCommit();
        if (marker.getViewTreeObserver().isAlive()) {
            marker.getViewTreeObserver().removeOnPreDrawListener(preDrawListener);
        }
        super.onDestroy();
    }

    private final class MarkerView extends View {
        private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
        private long event = sequence;
        private long drawn = -1;

        MarkerView() {
            super(MarkerActivity.this);
            setImportantForAccessibility(View.IMPORTANT_FOR_ACCESSIBILITY_YES);
            setContentDescription("イベント " + event);
            paint.setColor(Color.WHITE);
            paint.setTextAlign(Paint.Align.CENTER);
        }

        @Override
        protected void onDraw(Canvas canvas) {
            super.onDraw(canvas);
            canvas.drawColor((event & 1) == 0 ? Color.rgb(0, 65, 120) : Color.rgb(95, 0, 70));
            paint.setTextSize(Math.min(getWidth() / 4f, getHeight() / 3f));
            canvas.drawText(Long.toString(event), getWidth() / 2f, getHeight() / 2f, paint);
            if (drawn != event) {
                drawn = event;
                stamp("DRAW", event, viewId);
            }
        }
    }
}
