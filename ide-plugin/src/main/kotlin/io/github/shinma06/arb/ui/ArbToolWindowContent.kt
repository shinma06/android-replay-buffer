package io.github.shinma06.arb.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.VerticalScrollbar
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.defaultScrollbarStyle
import androidx.compose.foundation.hoverable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsHoveredAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.rememberScrollbarAdapter
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.shinma06.arb.ide.ArbNotifications
import io.github.shinma06.arb.model.BundleKind
import io.github.shinma06.arb.model.CaptureMode
import io.github.shinma06.arb.model.SaveJobView
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.BundleCatalogItem
import io.github.shinma06.arb.state.BundleFilter
import io.github.shinma06.arb.state.CaptureBanner
import io.github.shinma06.arb.state.CaptureBannerKind
import io.github.shinma06.arb.state.LEGACY_READ_ONLY_BADGE_TEXT
import org.jetbrains.jewel.foundation.theme.JewelTheme
import org.jetbrains.jewel.ui.Orientation
import org.jetbrains.jewel.ui.component.DefaultButton
import org.jetbrains.jewel.ui.component.Divider
import org.jetbrains.jewel.ui.component.Icon
import org.jetbrains.jewel.ui.component.IconActionButton
import org.jetbrains.jewel.ui.component.ListComboBox
import org.jetbrains.jewel.ui.component.Text
import org.jetbrains.jewel.ui.icons.AllIconsKeys
import org.jetbrains.jewel.ui.theme.scrollbarStyle

/**
 * Native JetBrains Jewel Compose UI for the `ARB` Tool Window (Cockpit & Library).
 *
 * Visual design rules:
 * - Zero outer card boxes or square borders; renders directly on `JewelTheme.globalColors.panelBackground`
 * - Native Jewel `ListComboBox` for device selection (no opaque black rectangle!)
 * - ONE clean Jewel `DefaultButton` primary CTA; all secondary utility & library actions are
 *   borderless hover-tinted flat controls matching Android Studio's Islands / IntUI chrome
 * - Zero emoji chrome anywhere; strict seconds notation (`272s / 300s`)
 */
@OptIn(ExperimentalLayoutApi::class, ExperimentalFoundationApi::class)
@Composable
fun ArbToolWindowContent(controller: ArbToolWindowController) {
    val capture = controller.captureState
    val recovery = controller.recoveryState
    val catalog = controller.catalogState
    val activeFilter = controller.activeFilter
    val staleMessage = controller.staleDaemonMessage

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(ArbJewelPalette.panelBackground()),
    ) {
        // ---- Cockpit (borderless, crisp native padding) ----
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 10.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // Row 1: 28dp height — Device ListComboBox + quiet inline status dot/text + borderless Settings gear
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .height(28.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                val labels = controller.deviceDropdownLabels
                val selectedIdx = controller.selectedDeviceIndex
                ArbTooltip(controller.deviceTooltip, modifier = Modifier.weight(1f)) {
                    key(labels, selectedIdx, controller.deviceDropdownEnabled) {
                        ListComboBox(
                            items = labels,
                            selectedIndex = selectedIdx,
                            onSelectedItemChange = { idx ->
                                if (controller.deviceDropdownEnabled) {
                                    controller.selectDeviceByIndex(idx)
                                }
                            },
                            enabled = controller.deviceDropdownEnabled,
                            modifier = Modifier
                                .fillMaxWidth()
                                .semantics { contentDescription = "対象端末" },
                        )
                    }
                }

                InlineStatusIndicator(
                    text = controller.captureStatusBadgeText,
                    isRecording = capture.isRecordingHealthy,
                    isArmedWaiting = controller.captureToggleState == CaptureToggleState.ARMED_WAITING,
                    isWarning = capture.capture?.storagePressureStopped == true ||
                        (capture.capture?.active == true && !capture.capture!!.connected) ||
                        (capture.capture?.active != true && capture.devices.isNotEmpty() && !capture.hasOnlineDevice),
                )

                IconActionButton(
                    key = AllIconsKeys.General.GearPlain,
                    contentDescription = "ARB 設定",
                    onClick = { controller.openSettings() },
                    modifier = Modifier.size(26.dp),
                ) {
                    Text("ARB 設定（保存秒数・CLI/MCP・スクリーンショット保存先）")
                }
            }

            // Armed-waiting notice when always-on capture is ON but waiting for a device / selection
            controller.armedWaitingNotice?.let { waitingText ->
                Text(
                    text = waitingText,
                    style = ArbJewelPalette.bodyStyle(sizeSp = 11f, color = ArbJewelPalette.accent()),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.semantics { contentDescription = waitingText },
                )
            }

            // Inline device-selection prompt (zero height unless the user must pick / reconnect)
            controller.deviceSelectionNotice?.let { notice ->
                Text(
                    text = notice,
                    style = ArbJewelPalette.bodyStyle(sizeSp = 11f, color = ArbJewelPalette.warningAmber()),
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.semantics { contentDescription = notice },
                )
            }

            // Row 1b: quiet capture-mode toggle (映像＋ログ / ログのみ) + `ログのみ常時` status chip
            CaptureModeRow(controller)

            // Row 2: Slim rounded ring fill indicator + muted tabular seconds & audio status
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                RingFillBar(
                    fillRatio = capture.ringFillRatio.toFloat(),
                    recording = capture.isRecordingHealthy,
                )
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Text(
                        text = capture.ringMeterText,
                        style = ArbJewelPalette.tabularMonoStyle(sizeSp = 11f, color = ArbJewelPalette.mutedText()),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        text = capture.audioStatusLabel,
                        style = ArbJewelPalette.bodyStyle(
                            sizeSp = 10.5f,
                            color = if (capture.config.audioEnabled) ArbJewelPalette.mutedText() else ArbJewelPalette.dimmedText(),
                        ),
                        maxLines = 1,
                    )
                }
            }

            // Row 3: Single clean Jewel DefaultButton Primary CTA (natural Android Studio height)
            DefaultButton(
                onClick = { controller.triggerPrimarySave() },
                enabled = capture.daemonConnected && !controller.saveBusy,
                modifier = Modifier
                    .fillMaxWidth()
                    .semantics { contentDescription = capture.primarySaveLabel },
            ) {
                Text(
                    text = capture.primarySaveLabel,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }

            // Row 4: Compact borderless utility row (zero 1px boxed rectangles!)
            FlowRow(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(2.dp),
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                ArbSubtleAction(
                    label = capture.secondarySaveLabel,
                    iconKey = AllIconsKeys.Actions.Download,
                    enabled = capture.daemonConnected && !controller.saveBusy,
                    tooltip = "エディタを開かず保存（または既定と逆の動作）",
                    compact = true,
                    onClick = { controller.triggerSecondarySave() },
                )
                ArbSubtleAction(
                    label = ArbFormatters.SECONDARY_SCREENSHOT_LABEL,
                    iconKey = AllIconsKeys.Actions.Dump,
                    enabled = controller.screenshotEnabled && !controller.screenshotBusy,
                    tooltip = controller.screenshotTooltip,
                    compact = true,
                    onClick = { controller.captureScreenshot() },
                )
                ArbSubtleAction(
                    label = ArbFormatters.SECONDARY_LOGS_ONLY_LABEL,
                    iconKey = AllIconsKeys.FileTypes.Text,
                    enabled = capture.daemonConnected && !controller.logSaveBusy,
                    tooltip = "映像キャプチャを中断せず直前のログのみ保存",
                    compact = true,
                    onClick = { controller.saveLogsOnly() },
                )
                ArbSubtleAction(
                    label = controller.startStopLabel,
                    iconKey = if (controller.isCaptureOn) AllIconsKeys.Actions.Suspend else AllIconsKeys.Actions.Execute,
                    enabled = controller.startStopEnabled,
                    tooltip = controller.startStopTooltip,
                    compact = true,
                    tint = when (controller.captureToggleState) {
                        CaptureToggleState.RECORDING -> ArbJewelPalette.recRed()
                        CaptureToggleState.ARMED_WAITING -> ArbJewelPalette.accent()
                        CaptureToggleState.OFF -> null
                    },
                    onClick = { controller.toggleCapture() },
                )
                ArbSubtleAction(
                    label = "トレイ",
                    iconKey = AllIconsKeys.Toolwindows.ToolWindowRun,
                    enabled = true,
                    tooltip = "メニューバー / システムトレイ (arb-tray) を起動",
                    compact = true,
                    onClick = { controller.launchTray() },
                )
            }

            // Conditional Tier-2 alert banners (zero height when healthy!)
            if (staleMessage != null) {
                StaleDaemonBanner(
                    message = staleMessage,
                    busy = controller.daemonRestartBusy,
                    onRestart = { controller.restartDaemon() },
                )
            }
            for (banner in capture.banners) {
                key(banner.kind, banner.title) { CaptureAlertBanner(banner) }
            }
            for (job in recovery.retryableJobs) {
                FailedSaveJobBanner(
                    job = job,
                    busy = recovery.busyJobId != null,
                    onRetry = { controller.retryFailedJob(job.jobId) },
                    onRetarget = { controller.retargetFailedJobInteractive(job.jobId) },
                    onDiscard = { controller.discardFailedJob(job.jobId) },
                )
            }
            recovery.lastMessage?.takeIf { it.isNotBlank() }?.let { msg ->
                Text(
                    text = msg,
                    style = ArbJewelPalette.bodyStyle(sizeSp = 11f, color = ArbJewelPalette.mutedText()),
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }

        // Subtle hairline divider separating cockpit from library
        Divider(
            orientation = Orientation.Horizontal,
            modifier = Modifier.fillMaxWidth(),
            color = ArbJewelPalette.hairlineDivider(),
        )

        // ---- Filter Row (delicate borderless inline tabs; GC shown ONLY in ゴミ箱 tab!) ----
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 8.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            FlowRow(
                modifier = Modifier.weight(1f),
                horizontalArrangement = Arrangement.spacedBy(2.dp),
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                for (filter in BundleFilter.entries) {
                    val count = catalog.countFor(filter)
                    val label = if (count > 0 && filter != BundleFilter.ALL) {
                        "${filter.label} ($count)"
                    } else {
                        filter.label
                    }
                    ArbFilterTab(
                        label = label,
                        selected = activeFilter == filter,
                        onClick = { controller.selectFilter(filter) },
                    )
                }
            }
            if (controller.showGcAction) {
                ArbSubtleAction(
                    label = if (controller.gcBusy) "削除中..." else "ゴミ箱を空にする",
                    iconKey = AllIconsKeys.Actions.GC,
                    enabled = controller.emptyTrashEnabled,
                    tooltip = "ゴミ箱内のバンドルを完全に削除（使用中・旧形式・外部保存先は保護）",
                    compact = true,
                    onClick = { controller.requestEmptyTrash() },
                )
            }
        }

        // Inline Compose/Jewel confirmation bar for Empty Trash (KON-3: zero Swing dialogs)
        if (controller.showEmptyTrashConfirmation) {
            Box(modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp)) {
                EmptyTrashConfirmBanner(
                    trashCount = controller.trashCount,
                    busy = controller.gcBusy,
                    onConfirm = { controller.confirmEmptyTrash() },
                    onCancel = { controller.cancelEmptyTrash() },
                )
            }
        }

        // Inline bulk-cleanup banner when manually deleted/moved bundles exist in catalog
        if (controller.missingBundleCount > 0) {
            Box(modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp)) {
                MissingBundlesBanner(
                    missingCount = controller.missingBundleCount,
                    enabled = capture.daemonConnected && !controller.forgetBusy,
                    busy = controller.forgetBusy,
                    onPruneAll = { controller.forgetAllMissingBundles() },
                )
            }
        }

        // ---- Outer-borderless flat LazyColumn Library ----
        val items = controller.displayedBundles
        Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
            if (items.isEmpty()) {
                Box(
                    modifier = Modifier.fillMaxSize().padding(16.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = if (activeFilter == BundleFilter.TRASH) {
                            "ゴミ箱は空です"
                        } else {
                            "保存済みバンドルはありません"
                        },
                        style = ArbJewelPalette.bodyStyle(sizeSp = 11.5f, color = ArbJewelPalette.mutedText()),
                    )
                }
            } else {
                val listState = rememberLazyListState()
                val jewelSb = JewelTheme.scrollbarStyle
                LazyColumn(
                    state = listState,
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(horizontal = 6.dp),
                    verticalArrangement = Arrangement.spacedBy(2.dp),
                ) {
                    items(items, key = { it.bundleId }) { item ->
                        LibraryBundleRow(
                            item = item,
                            forgetBusy = controller.forgetBusy,
                            onOpen = { controller.openBundle(item) },
                            onReveal = { controller.revealInFinder(item.path) },
                            onTrash = { controller.trashBundle(item.bundleId) },
                            onRestore = { controller.restoreBundle(item.bundleId) },
                            onForget = { controller.forgetBundle(item.bundleId) },
                        )
                    }
                }
                VerticalScrollbar(
                    adapter = rememberScrollbarAdapter(listState),
                    modifier = Modifier.align(Alignment.CenterEnd).fillMaxHeight(),
                    style = defaultScrollbarStyle().copy(
                        thickness = jewelSb.scrollbarVisibility.trackThickness,
                        shape = RoundedCornerShape(jewelSb.metrics.thumbCornerSize),
                        unhoverColor = jewelSb.colors.thumbOpaqueBackground.copy(alpha = 0.45f),
                        hoverColor = jewelSb.colors.thumbBackgroundActive,
                    ),
                )
            }
        }

        // Quiet single-line footer hint (only when a catalog status note exists)
        controller.catalogNote?.takeIf { it.isNotBlank() }?.let { note ->
            Text(
                text = note,
                style = ArbJewelPalette.bodyStyle(sizeSp = 10.5f, color = ArbJewelPalette.mutedText()),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
            )
        }
    }
}

@Composable
private fun CaptureModeRow(controller: ArbToolWindowController) {
    val mode = controller.captureMode
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Text(
            text = "モード",
            style = ArbJewelPalette.bodyStyle(sizeSp = 11f, color = ArbJewelPalette.mutedText()),
            modifier = Modifier.padding(end = 4.dp),
        )
        for (option in CaptureMode.entries) {
            ArbTooltip(
                when (option) {
                    CaptureMode.VIDEO_AND_LOGS -> "画面録画・音声・ログ・タップを常時記録"
                    CaptureMode.LOGS_ONLY -> "映像を止めてログ・クラッシュ・画面遷移のみを低負荷で常時記録"
                },
            ) {
                ArbFilterTab(
                    label = ArbToolWindowController.captureModeLabel(option),
                    selected = mode == option,
                    enabled = controller.captureModeToggleEnabled,
                    onClick = { controller.setCaptureMode(option) },
                )
            }
        }
        Box(modifier = Modifier.weight(1f))
        controller.captureModeChipText?.let { chip ->
            ArbSemanticPill(
                label = chip,
                background = ArbJewelPalette.infoCyanSoft(),
                foreground = ArbJewelPalette.infoCyan(),
                tooltip = "ログのみモードで常時記録中（端末側の映像エンコード負荷なし）",
            )
        }
    }
}

@Composable
private fun InlineStatusIndicator(
    text: String,
    isRecording: Boolean,
    isArmedWaiting: Boolean = false,
    isWarning: Boolean,
) {
    val dotColor = when {
        isRecording -> ArbJewelPalette.recRed()
        isWarning -> ArbJewelPalette.warningAmber()
        isArmedWaiting -> ArbJewelPalette.accent()
        else -> ArbJewelPalette.mutedText().copy(alpha = 0.6f)
    }
    val textColor = when {
        isRecording -> ArbJewelPalette.recRed()
        isWarning -> ArbJewelPalette.warningAmber()
        isArmedWaiting -> ArbJewelPalette.accent()
        else -> ArbJewelPalette.mutedText()
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier.padding(horizontal = 2.dp),
    ) {
        Box(
            Modifier
                .size(6.dp)
                .clip(CircleShape)
                .background(dotColor),
        )
        Text(
            text = text,
            style = ArbJewelPalette.tabularMonoStyle(sizeSp = 10.5f, bold = isRecording || isArmedWaiting, color = textColor),
            maxLines = 1,
            softWrap = false,
        )
    }
}

@Composable
private fun RingFillBar(
    fillRatio: Float,
    recording: Boolean,
) {
    val trackColor = ArbJewelPalette.subtleTrackFill()
    val fillColor = if (recording) ArbJewelPalette.accent() else ArbJewelPalette.mutedText().copy(alpha = 0.55f)
    Canvas(
        modifier = Modifier
            .fillMaxWidth()
            .height(4.dp),
    ) {
        val r = CornerRadius(size.height / 2f, size.height / 2f)
        drawRoundRect(color = trackColor, cornerRadius = r)
        val clamped = fillRatio.coerceIn(0f, 1f)
        if (clamped > 0f) {
            drawRoundRect(
                color = fillColor,
                topLeft = Offset.Zero,
                size = Size(size.width * clamped, size.height),
                cornerRadius = r,
            )
        }
    }
}

@Composable
private fun StaleDaemonBanner(
    message: String,
    busy: Boolean,
    onRestart: () -> Unit,
) {
    ArbSoftBanner(
        background = ArbJewelPalette.warningAmberSoft(),
        accent = ArbJewelPalette.warningAmber(),
    ) {
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                text = message,
                style = ArbJewelPalette.bodyStyle(sizeSp = 11f, color = ArbJewelPalette.primaryText()),
            )
            ArbSubtleAction(
                label = if (busy) "再起動中..." else "arbd を再起動",
                iconKey = AllIconsKeys.Actions.Restart,
                enabled = !busy,
                compact = true,
                tint = ArbJewelPalette.warningAmber(),
                onClick = onRestart,
            )
        }
    }
}

@Composable
internal fun CaptureAlertBanner(
    banner: CaptureBanner,
    onCopy: (String) -> Unit = ArbNotifications::copyTextToClipboard,
) {
    val (bg, accent) = when (banner.kind) {
        CaptureBannerKind.STORAGE_PRESSURE_STOP -> ArbJewelPalette.recRedSoft() to ArbJewelPalette.recRed()
        CaptureBannerKind.CONNECTION_LOST,
        CaptureBannerKind.STREAM_GAP,
        CaptureBannerKind.CONFIG_WARNING,
        -> ArbJewelPalette.warningAmberSoft() to ArbJewelPalette.warningAmber()
        CaptureBannerKind.STATIC_HOLD -> ArbJewelPalette.infoCyanSoft() to ArbJewelPalette.infoCyan()
    }
    ArbSoftBanner(background = bg, accent = accent) {
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                text = banner.title,
                style = ArbJewelPalette.bodyStyle(sizeSp = 11f, bold = true, color = ArbJewelPalette.primaryText()),
            )
            Text(
                text = banner.detail,
                style = ArbJewelPalette.bodyStyle(sizeSp = 10.5f, color = ArbJewelPalette.mutedText()),
            )
            val raw = banner.technicalDetail
            if (!raw.isNullOrEmpty()) {
                var expanded by remember(banner) { mutableStateOf(false) }
                Row {
                    ArbSubtleAction(
                        label = if (expanded) "詳細を隠す" else "詳細を表示",
                        compact = true,
                        onClick = { expanded = !expanded },
                    )
                    if (expanded) {
                        ArbSubtleAction(label = "詳細をコピー", compact = true, onClick = { onCopy(raw) })
                    }
                }
                if (expanded) {
                    Text(
                        text = raw,
                        modifier = Modifier.heightIn(max = 160.dp).verticalScroll(rememberScrollState()),
                        style = ArbJewelPalette.tabularMonoStyle(sizeSp = 10.5f),
                    )
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun FailedSaveJobBanner(
    job: SaveJobView,
    busy: Boolean,
    onRetry: () -> Unit,
    onRetarget: () -> Unit,
    onDiscard: () -> Unit,
) {
    ArbSoftBanner(
        background = ArbJewelPalette.recRedSoft(),
        accent = ArbJewelPalette.recRed(),
    ) {
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                text = "保存失敗 (${job.jobId}) — チャンク ${job.pinnedChunkIds.size} 件を保護中",
                style = ArbJewelPalette.bodyStyle(sizeSp = 11f, bold = true, color = ArbJewelPalette.primaryText()),
            )
            Text(
                text = "保存先: ${job.destinationPath} · ${job.lastError ?: "書き込みエラー"}",
                style = ArbJewelPalette.bodyStyle(sizeSp = 10.5f, color = ArbJewelPalette.mutedText()),
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                ArbSubtleAction(
                    label = "再試行",
                    iconKey = AllIconsKeys.Actions.Refresh,
                    enabled = !busy,
                    compact = true,
                    onClick = onRetry,
                )
                ArbSubtleAction(
                    label = "保存先変更",
                    iconKey = AllIconsKeys.Nodes.Folder,
                    enabled = !busy,
                    compact = true,
                    onClick = onRetarget,
                )
                ArbSubtleAction(
                    label = "破棄",
                    iconKey = AllIconsKeys.Actions.Cancel,
                    enabled = !busy,
                    compact = true,
                    tint = ArbJewelPalette.recRed(),
                    onClick = onDiscard,
                )
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun EmptyTrashConfirmBanner(
    trashCount: Int,
    busy: Boolean,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
) {
    ArbSoftBanner(
        background = ArbJewelPalette.recRedSoft(),
        accent = ArbJewelPalette.recRed(),
    ) {
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                text = "ゴミ箱の $trashCount 件を完全に削除しますか？この操作は取り消せません。",
                style = ArbJewelPalette.bodyStyle(sizeSp = 11f, bold = true, color = ArbJewelPalette.primaryText()),
            )
            FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                ArbSubtleAction(
                    label = if (busy) "削除中..." else "完全に削除",
                    iconKey = AllIconsKeys.Actions.GC,
                    enabled = !busy,
                    compact = true,
                    tint = ArbJewelPalette.recRed(),
                    onClick = onConfirm,
                )
                ArbSubtleAction(
                    label = "キャンセル",
                    enabled = !busy,
                    compact = true,
                    onClick = onCancel,
                )
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun MissingBundlesBanner(
    missingCount: Int,
    enabled: Boolean,
    busy: Boolean,
    onPruneAll: () -> Unit,
) {
    ArbSoftBanner(
        background = ArbJewelPalette.warningAmberSoft(),
        accent = ArbJewelPalette.warningAmber(),
    ) {
        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                text = "フォルダが見つからないバンドルが $missingCount 件あります（手動削除・移動済み）",
                style = ArbJewelPalette.bodyStyle(sizeSp = 11f, color = ArbJewelPalette.primaryText()),
            )
            FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                ArbSubtleAction(
                    label = if (busy) "整理中..." else "一覧から一括整理",
                    iconKey = AllIconsKeys.Actions.GC,
                    enabled = enabled,
                    compact = true,
                    tint = ArbJewelPalette.warningAmber(),
                    tooltip = "ディスクに存在しないバンドルをカタログ一覧から取り除きます（ファイル削除は行いません）",
                    onClick = onPruneAll,
                )
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun LibraryBundleRow(
    item: BundleCatalogItem,
    forgetBusy: Boolean,
    onOpen: () -> Unit,
    onReveal: () -> Unit,
    onTrash: () -> Unit,
    onRestore: () -> Unit,
    onForget: () -> Unit,
) {
    val interactionSource = remember { MutableInteractionSource() }
    val hovered by interactionSource.collectIsHoveredAsState()
    val shape = RoundedCornerShape(6.dp)
    val rowBg = if (hovered) ArbJewelPalette.subtleHoverFill() else Color.Transparent
    val iconKey = when {
        item.isMissingOnDisk || item.isStorageUnavailable -> AllIconsKeys.General.Warning
        item.hasCrash -> AllIconsKeys.General.Error
        item.bundleKind == BundleKind.LOGS_ONLY -> AllIconsKeys.FileTypes.Text
        item.isTrimmed -> AllIconsKeys.Actions.Edit
        else -> AllIconsKeys.Actions.Execute
    }
    val titleColor = when {
        item.isMissingOnDisk -> ArbJewelPalette.dimmedText()
        item.isStorageUnavailable -> ArbJewelPalette.mutedText()
        else -> ArbJewelPalette.primaryText()
    }

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(rowBg, shape)
            .hoverable(interactionSource)
            .clickable(
                interactionSource = interactionSource,
                indication = null,
                role = Role.Button,
                onClick = onOpen,
            )
            .padding(horizontal = 8.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Icon(
            key = iconKey,
            contentDescription = null,
            modifier = Modifier.size(15.dp),
        )

        Column(
            modifier = Modifier.weight(1f),
            verticalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            Text(
                text = item.displayTitle,
                style = ArbJewelPalette.bodyStyle(sizeSp = 12f, bold = false, color = titleColor),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                val timeShort = item.createdAtUtc.replace('T', ' ').removeSuffix("Z").take(19)
                Text(
                    text = "$timeShort · ${item.durationText}",
                    style = ArbJewelPalette.tabularMonoStyle(sizeSp = 10.5f, color = ArbJewelPalette.mutedText()),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                for (tag in item.semanticTags.take(2)) {
                    val (bg, fg) = when (tag) {
                        "クラッシュ" -> ArbJewelPalette.recRedSoft() to ArbJewelPalette.recRed()
                        "トリム済み" -> ArbJewelPalette.accentSoft() to ArbJewelPalette.accent()
                        BundleCatalogItem.MISSING_ON_DISK_BADGE_TEXT,
                        BundleCatalogItem.STORAGE_UNAVAILABLE_BADGE_TEXT,
                        BundleCatalogItem.CRASH_STATUS_UNKNOWN_BADGE_TEXT,
                        LEGACY_READ_ONLY_BADGE_TEXT,
                        -> ArbJewelPalette.warningAmberSoft() to ArbJewelPalette.warningAmber()
                        else -> ArbJewelPalette.subtleTrackFill() to ArbJewelPalette.mutedText()
                    }
                    ArbSemanticPill(label = tag, background = bg, foreground = fg)
                }
            }
        }

        // Subtle inline hover actions (borderless icons — no noisy buttons at rest!)
        if (item.canForget) {
            ArbSubtleAction(
                label = "一覧から削除",
                iconKey = AllIconsKeys.Actions.Cancel,
                enabled = !forgetBusy,
                compact = true,
                tint = ArbJewelPalette.warningAmber(),
                tooltip = "見つからないバンドルをカタログ一覧から取り除く（ディスク操作なし）",
                onClick = onForget,
            )
        } else if (hovered) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                if (item.canOpen) {
                    IconActionButton(
                        key = AllIconsKeys.Nodes.Folder,
                        contentDescription = "フォルダを表示",
                        onClick = onReveal,
                        modifier = Modifier.size(22.dp),
                    ) {
                        Text("フォルダを表示")
                    }
                }
                if (item.canTrash) {
                    IconActionButton(
                        key = AllIconsKeys.Actions.Cancel,
                        contentDescription = "ゴミ箱へ",
                        onClick = onTrash,
                        modifier = Modifier.size(22.dp),
                    ) {
                        Text("ゴミ箱へ移動")
                    }
                }
                if (item.canRestore) {
                    IconActionButton(
                        key = AllIconsKeys.Actions.Rollback,
                        contentDescription = "復元",
                        onClick = onRestore,
                        modifier = Modifier.size(22.dp),
                    ) {
                        Text("ゴミ箱から復元")
                    }
                }
            }
        }
    }
}
