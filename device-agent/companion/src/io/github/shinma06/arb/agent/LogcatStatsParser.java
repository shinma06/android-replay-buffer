package io.github.shinma06.arb.agent;

import java.util.LinkedHashMap;
import java.util.Map;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Parses {@code logcat -b all -S} (and optional {@code logcat -g}) statistics output to assess
 * logd completeness honestly per REQ-03:
 * <ul>
 *   <li>{@link Protocol#COMPLETENESS_VERIFIED}: {@code logcat -S} parsed cleanly AND zero ring-buffer
 *       evictions occurred during the interval AND zero reader errors/EOFs occurred.</li>
 *   <li>{@link Protocol#COMPLETENESS_OVERFLOW_DETECTED}: explicit dropped/overflow condition detected
 *       (e.g. logcat EOF/exit, explicit dropped counter increase, or ring wrap exceeding retained capacity).</li>
 *   <li>{@link Protocol#COMPLETENESS_UNVERIFIED}: stats unavailable/unparseable, or ring-buffer pruning
 *       occurred without a per-client drop counter proving zero reader loss.</li>
 * </ul>
 */
public final class LogcatStatsParser {
    private static final Pattern PAIR_PATTERN = Pattern.compile("(\\d+)/(\\d+)");
    private static final Pattern DROPPED_PATTERN = Pattern.compile("(?i)(?:dropped|lost)\\s*[:=]?\\s*(\\d+)");

    private LogcatStatsParser() {}

    public static final class Snapshot {
        public final boolean valid;
        public final int bufferCount;
        public final long totalEntries;
        public final long nowEntries;
        public final long prunedCumulative;
        public final long minBufferNowEntries;
        public final long explicitDropped;
        public final Map<String, Long> perBufferPruned;
        public final String summary;

        public Snapshot(
                boolean valid,
                int bufferCount,
                long totalEntries,
                long nowEntries,
                long prunedCumulative,
                long minBufferNowEntries,
                long explicitDropped,
                Map<String, Long> perBufferPruned,
                String summary) {
            this.valid = valid;
            this.bufferCount = bufferCount;
            this.totalEntries = totalEntries;
            this.nowEntries = nowEntries;
            this.prunedCumulative = prunedCumulative;
            this.minBufferNowEntries = minBufferNowEntries;
            this.explicitDropped = explicitDropped;
            this.perBufferPruned = perBufferPruned;
            this.summary = summary;
        }

        public static Snapshot invalid(String reason) {
            return new Snapshot(false, 0, -1, -1, -1, -1, -1, new LinkedHashMap<String, Long>(), reason);
        }
    }

    /**
     * Parses the header table from {@code logcat -b all -S} or {@code logcat -S}:
     * <pre>
     * size/num main               system             crash              kernel             Total
     * Total    5589250/35817      2309367/13067      0/0                321952/2774        8220569/51658
     * Now      5327600/33794      2309367/13067      0/0                321952/2774        7958919/49635
     * </pre>
     */
    public static Snapshot parseLogcatS(String output) {
        if (output == null || output.trim().isEmpty()) {
            return Snapshot.invalid("empty_logcat_S_output");
        }

        String[] lines = output.split("\\r?\\n");
        String[] bufferNames = null;
        long[] totalCounts = null;
        long[] nowCounts = null;
        long explicitDropped = 0;

        for (String rawLine : lines) {
            String line = rawLine.trim();
            if (line.isEmpty()) {
                continue;
            }
            if (line.startsWith("size/num")) {
                String[] cols = line.split("\\s+");
                if (cols.length >= 2) {
                    bufferNames = new String[cols.length - 1];
                    System.arraycopy(cols, 1, bufferNames, 0, cols.length - 1);
                }
            } else if (line.startsWith("Total") && totalCounts == null) {
                totalCounts = parseEntryCounts(line.substring("Total".length()));
            } else if (line.startsWith("Now") && nowCounts == null) {
                nowCounts = parseEntryCounts(line.substring("Now".length()));
            } else {
                Matcher dm = DROPPED_PATTERN.matcher(line);
                if (dm.find()) {
                    try {
                        explicitDropped += Long.parseLong(dm.group(1));
                    } catch (NumberFormatException ignored) {
                    }
                }
            }
        }

        if (bufferNames == null || totalCounts == null || nowCounts == null) {
            return Snapshot.invalid("missing_size_num_table");
        }

        int n = Math.min(bufferNames.length, Math.min(totalCounts.length, nowCounts.length));
        if (n == 0) {
            return Snapshot.invalid("zero_buffers_parsed");
        }

        // Exclude trailing aggregate "Total" column if present so we do not double-count.
        int effectiveCount = n;
        if (n > 1 && "Total".equalsIgnoreCase(bufferNames[n - 1])) {
            effectiveCount = n - 1;
        }

        long sumTotal = 0;
        long sumNow = 0;
        long sumPruned = 0;
        long minNonZeroNow = Long.MAX_VALUE;
        Map<String, Long> perBuf = new LinkedHashMap<>();
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < effectiveCount; i++) {
            String name = bufferNames[i];
            long t = totalCounts[i];
            long nw = nowCounts[i];
            long pruned = Math.max(0L, t - nw);
            sumTotal += t;
            sumNow += nw;
            sumPruned += pruned;
            if (nw > 0 && nw < minNonZeroNow) {
                minNonZeroNow = nw;
            }
            perBuf.put(name, pruned);
            if (sb.length() > 0) {
                sb.append(' ');
            }
            sb.append(name).append(':').append(pruned);
        }
        if (minNonZeroNow == Long.MAX_VALUE) {
            minNonZeroNow = 0;
        }
        return new Snapshot(
                true,
                effectiveCount,
                sumTotal,
                sumNow,
                sumPruned,
                minNonZeroNow,
                explicitDropped,
                perBuf,
                sb.toString());
    }

    /**
     * Evaluates two consecutive snapshots (or the initial snapshot when {@code prev == null})
     * into a wire {@link PayloadCodec.LogOverflowStats} payload.
     */
    public static PayloadCodec.LogOverflowStats evaluate(
            long boottimeNs,
            Snapshot prev,
            Snapshot curr,
            long readerEmittedDelta,
            boolean hadReaderGap) {
        if (hadReaderGap) {
            return new PayloadCodec.LogOverflowStats(
                    boottimeNs,
                    Protocol.COMPLETENESS_OVERFLOW_DETECTED,
                    curr.valid ? curr.bufferCount : 0,
                    curr.valid && prev != null && prev.valid
                            ? Math.max(0L, curr.prunedCumulative - prev.prunedCumulative)
                            : -1L,
                    curr.valid ? curr.prunedCumulative : -1L,
                    "reader_gap_detected");
        }
        if (curr == null || !curr.valid) {
            String reason = curr != null ? curr.summary : "null_snapshot";
            return new PayloadCodec.LogOverflowStats(
                    boottimeNs,
                    Protocol.COMPLETENESS_UNVERIFIED,
                    0,
                    -1L,
                    -1L,
                    "unverified:" + reason);
        }

        if (prev == null || !prev.valid) {
            // Initial baseline probe at session startup: no prior interval to prove zero loss yet,
            // unless cumulative pruned is 0 across all buffers.
            if (curr.prunedCumulative == 0 && curr.explicitDropped == 0) {
                return new PayloadCodec.LogOverflowStats(
                        boottimeNs,
                        Protocol.COMPLETENESS_VERIFIED,
                        curr.bufferCount,
                        0L,
                        0L,
                        "baseline_zero_pruned " + curr.summary);
            }
            return new PayloadCodec.LogOverflowStats(
                    boottimeNs,
                    Protocol.COMPLETENESS_UNVERIFIED,
                    curr.bufferCount,
                    0L,
                    curr.prunedCumulative,
                    "baseline_preexisting_pruned=" + curr.prunedCumulative + " " + curr.summary);
        }

        long prunedDelta = Math.max(0L, curr.prunedCumulative - prev.prunedCumulative);
        long totalDelta = Math.max(0L, curr.totalEntries - prev.totalEntries);
        long droppedDelta = Math.max(0L, curr.explicitDropped - prev.explicitDropped);

        if (droppedDelta > 0) {
            return new PayloadCodec.LogOverflowStats(
                    boottimeNs,
                    Protocol.COMPLETENESS_OVERFLOW_DETECTED,
                    curr.bufferCount,
                    prunedDelta + droppedDelta,
                    curr.prunedCumulative + curr.explicitDropped,
                    "explicit_dropped_delta=" + droppedDelta + " " + curr.summary);
        }

        if (prunedDelta == 0) {
            return new PayloadCodec.LogOverflowStats(
                    boottimeNs,
                    Protocol.COMPLETENESS_VERIFIED,
                    curr.bufferCount,
                    0L,
                    curr.prunedCumulative,
                    "zero_pruned_delta total_delta=" + totalDelta
                            + " reader_delta=" + readerEmittedDelta + " " + curr.summary);
        }

        // Check if ring eviction outpaced the retained ring buffer capacity during this interval.
        if (curr.minBufferNowEntries > 0 && prunedDelta >= curr.minBufferNowEntries) {
            return new PayloadCodec.LogOverflowStats(
                    boottimeNs,
                    Protocol.COMPLETENESS_OVERFLOW_DETECTED,
                    curr.bufferCount,
                    prunedDelta,
                    curr.prunedCumulative,
                    "ring_wrap_exceeded_capacity pruned_delta=" + prunedDelta
                            + " total_delta=" + totalDelta + " reader_delta=" + readerEmittedDelta);
        }

        // Normal ring eviction happened in logd while streaming; because logcat -S does not expose
        // a per-client socket drop counter, honestly report Unverified rather than falsely Verified.
        return new PayloadCodec.LogOverflowStats(
                boottimeNs,
                Protocol.COMPLETENESS_UNVERIFIED,
                curr.bufferCount,
                prunedDelta,
                curr.prunedCumulative,
                "ring_pruned_delta=" + prunedDelta + " total_delta=" + totalDelta
                        + " reader_delta=" + readerEmittedDelta + " no_per_client_drop_counter");
    }

    private static long[] parseEntryCounts(String rest) {
        Matcher m = PAIR_PATTERN.matcher(rest);
        long[] tmp = new long[16];
        int count = 0;
        while (m.find() && count < tmp.length) {
            try {
                tmp[count++] = Long.parseLong(m.group(2));
            } catch (NumberFormatException ignored) {
            }
        }
        long[] res = new long[count];
        System.arraycopy(tmp, 0, res, 0, count);
        return res;
    }
}
