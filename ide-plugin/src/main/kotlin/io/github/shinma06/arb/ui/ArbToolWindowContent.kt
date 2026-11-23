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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollbarAdapter
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.remember
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
import io.github.shinma06.arb.model.BundleKind
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
                    text = capture.statusBadgeText,
                    isRecording = capture.isRecordingHealthy,
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
                        text = if (capture.config.audioEnabled) "音声 ON" else "音声 OFF",
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
                enabled = capture.daemonConnected,
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
                    enabled = capture.daemonConnected,
                    tooltip = "エディタを開かず保存（または既定と逆の動作）",
                    compact = true,
                    onClick = { controller.triggerSecondarySave() },
                )
                ArbSubtleAction(
                    label = ArbFormatters.SECONDARY_SCREENSHOT_LABEL,
                    iconKey = AllIconsKeys.Actions.Dump,
                    enabled = controller.screenshotEnabled,
                    tooltip = controller.screenshotTooltip,
                    compact = true,
                    onClick = { controller.captureScreenshot() },
                )
                ArbSubtleAction(
                    label = ArbFormatters.SECONDARY_LOGS_ONLY_LABEL,
                    iconKey = AllIconsKeys.FileTypes.Text,
                    enabled = capture.daemonConnected,
                    tooltip = "映像キャプチャを中断せず直前のログのみ保存",
                    compact = true,
                    onClick = { controller.saveLogsOnly() },
                )
                ArbSubtleAction(
                    label = controller.startStopLabel,
                    iconKey = if (controller.isCaptureActive) AllIconsKeys.Actions.Suspend else AllIconsKeys.Actions.Execute,
                    enabled = controller.startStopEnabled,
                    tooltip = controller.startStopTooltip,
                    compact = true,
                    tint = if (controller.isCaptureActive) ArbJewelPalette.recRed() else null,
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
                CaptureAlertBanner(banner)
            }
            for (job in recovery.retryableJobs) {
                FailedSaveJobBanner(
                    job = job,
                    busy = recovery.busyJobId == job.jobId,
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
                    label = "ゴミ箱を空にする",
                    iconKey = AllIconsKeys.Actions.GC,
                    tooltip = "猶予期間経過済みのゴミ箱バンドルを整理 (GC)",
                    compact = true,
                    onClick = { controller.runGc() },
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
                            onOpen = { controller.openBundle(item) },
                            onReveal = { controller.revealInFinder(item.path) },
                            onTrash = { controller.trashBundle(item.bundleId) },
                            onRestore = { controller.restoreBundle(item.bundleId) },
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
private fun InlineStatusIndicator(
    text: String,
    isRecording: Boolean,
    isWarning: Boolean,
) {
    val dotColor = when {
        isRecording -> ArbJewelPalette.recRed()
        isWarning -> ArbJewelPalette.warningAmber()
        else -> ArbJewelPalette.mutedText().copy(alpha = 0.6f)
    }
    val textColor = when {
        isRecording -> ArbJewelPalette.recRed()
        isWarning -> ArbJewelPalette.warningAmber()
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
            style = ArbJewelPalette.tabularMonoStyle(sizeSp = 10.5f, bold = isRecording, color = textColor),
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
private fun CaptureAlertBanner(banner: CaptureBanner) {
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

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun LibraryBundleRow(
    item: BundleCatalogItem,
    onOpen: () -> Unit,
    onReveal: () -> Unit,
    onTrash: () -> Unit,
    onRestore: () -> Unit,
) {
    val interactionSource = remember { MutableInteractionSource() }
    val hovered by interactionSource.collectIsHoveredAsState()
    val shape = RoundedCornerShape(6.dp)
    val rowBg = if (hovered) ArbJewelPalette.subtleHoverFill() else Color.Transparent
    val iconKey = when {
        item.hasCrash -> AllIconsKeys.General.Error
        item.bundleKind == BundleKind.LOGS_ONLY -> AllIconsKeys.FileTypes.Text
        item.isTrimmed -> AllIconsKeys.Actions.Edit
        else -> AllIconsKeys.Actions.Execute
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
                style = ArbJewelPalette.bodyStyle(sizeSp = 12f, bold = false, color = ArbJewelPalette.primaryText()),
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
                        LEGACY_READ_ONLY_BADGE_TEXT -> ArbJewelPalette.warningAmberSoft() to ArbJewelPalette.warningAmber()
                        else -> ArbJewelPalette.subtleTrackFill() to ArbJewelPalette.mutedText()
                    }
                    ArbSemanticPill(label = tag, background = bg, foreground = fg)
                }
            }
        }

        // Subtle inline hover actions (borderless icons — no noisy buttons at rest!)
        if (hovered) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                IconActionButton(
                    key = AllIconsKeys.Nodes.Folder,
                    contentDescription = "Finder で表示",
                    onClick = onReveal,
                    modifier = Modifier.size(22.dp),
                ) {
                    Text("Finder で表示")
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
