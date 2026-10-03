package io.github.shinma06.replaybuffer.fixture;

import android.app.Activity;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.os.Bundle;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;
import android.view.View;
import android.view.ViewTreeObserver;
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
    private MarkerView marker;
    private Runnable commitCallback;

    @Override
    public void onCreate(Bundle state) {
        super.onCreate(state);
        LinearLayout layout = new LinearLayout(this);
        layout.setOrientation(LinearLayout.VERTICAL);
        layout.setPadding(16, 48, 16, 16);
        TextView identity = new TextView(this);
        identity.setText("Replay QA\n" + BuildConfig.APPLICATION_ID + "\n"
                + BuildConfig.FIXTURE_SOURCE + "\nrun=" + RUN);
        layout.addView(identity);
        marker = new MarkerView();
        layout.addView(marker, new LinearLayout.LayoutParams(-1, 0, 1));
        Button emit = new Button(this);
        emit.setText("イベント番号を更新");
        emit.setOnClickListener(view -> {
            unregisterCommit();
            long event = ++sequence;
            stamp("REQUEST", event);
            marker.event = event;
            marker.setContentDescription("イベント " + event);
            marker.invalidate();
        });
        layout.addView(emit);
        setContentView(layout);
        stamp("CREATE", sequence);
    }

    private static void stamp(String phase, long event) {
        long before = SystemClock.elapsedRealtimeNanos();
        long wall = System.currentTimeMillis();
        long after = SystemClock.elapsedRealtimeNanos();
        try {
            JSONObject message = new JSONObject();
            message.put("schema", 1);
            message.put("package", BuildConfig.APPLICATION_ID);
            message.put("source", BuildConfig.FIXTURE_SOURCE);
            message.put("run", RUN);
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
                stamp("DRAW", event);
                if (isHardwareAccelerated()) {
                    long committedEvent = event;
                    commitCallback = () -> stamp("FRAME_COMMIT", committedEvent);
                    ViewTreeObserver observer = getViewTreeObserver();
                    observer.registerFrameCommitCallback(commitCallback);
                } else {
                    stamp("NO_HARDWARE_COMMIT", event);
                }
            }
        }
    }
}
