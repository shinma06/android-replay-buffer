package io.github.shinma06.arb.state

import io.github.shinma06.arb.model.AdbDeviceInfo
import io.github.shinma06.arb.model.AdbDeviceState

/**
 * Outcome of evaluating ARB's strict multi-device target selection policy.
 *
 * Mirrors Rust `arb_core::device::DeviceSelectionOutcome` identically across GUI, CLI, and daemon:
 * - No preferred serial + 0 online -> [NoOnlineDevices]
 * - No preferred serial + 1 online -> [Selected] (`autoSelected = true`)
 * - No preferred serial + 2+ online -> [MultipleDevicesNeedSelection] (NEVER auto-select!)
 * - Preferred serial set + online -> [Selected] (`autoSelected = false`)
 * - Preferred serial set + offline/missing -> [WaitingForPreferredDevice] (NEVER auto-switch!)
 */
sealed class DeviceSelectionOutcome {
    data class Selected(
        val serial: String,
        val autoSelected: Boolean,
    ) : DeviceSelectionOutcome()

    data object NoOnlineDevices : DeviceSelectionOutcome()

    data class MultipleDevicesNeedSelection(
        val onlineSerials: List<String>,
    ) : DeviceSelectionOutcome()

    data class WaitingForPreferredDevice(
        val preferredSerial: String,
        val otherOnlineSerials: List<String>,
    ) : DeviceSelectionOutcome()
}

/**
 * Shared pure policy enforcing ARB's strict multi-device no-auto-switch invariant and Wi-Fi /
 * USB / mDNS ADB serial validation.
 */
object DeviceSelectionPolicy {
    /** Mirrors Rust `arb_core::device::MAX_ADB_SERIAL_LEN` (bytes; serials are ASCII-only). */
    private const val MAX_ADB_SERIAL_LEN: Int = 255

    /**
     * Evaluates the target device given an optional [preferredSerial] and the currently visible
     * [devices] from `adb devices`.
     */
    fun evaluate(
        preferredSerial: String?,
        devices: List<AdbDeviceInfo>,
    ): DeviceSelectionOutcome {
        val onlineSerials = devices
            .filter { it.state == AdbDeviceState.Device }
            .map { it.serial }
        return evaluateOnlineSerials(preferredSerial, onlineSerials)
    }

    /** Evaluates the policy directly against a list of online (`device` state) ADB serials. */
    fun evaluateOnlineSerials(
        preferredSerial: String?,
        onlineSerials: List<String>,
    ): DeviceSelectionOutcome {
        val onlineSerials = onlineSerials.map { it.trim() }.filter { it.isNotEmpty() }
        val normalizedPreferred = preferredSerial?.trim()?.takeIf { it.isNotEmpty() }
        if (normalizedPreferred != null) {
            return if (normalizedPreferred in onlineSerials) {
                DeviceSelectionOutcome.Selected(serial = normalizedPreferred, autoSelected = false)
            } else {
                DeviceSelectionOutcome.WaitingForPreferredDevice(
                    preferredSerial = normalizedPreferred,
                    otherOnlineSerials = onlineSerials,
                )
            }
        }
        return when (onlineSerials.size) {
            0 -> DeviceSelectionOutcome.NoOnlineDevices
            1 -> DeviceSelectionOutcome.Selected(serial = onlineSerials.first(), autoSelected = true)
            else -> DeviceSelectionOutcome.MultipleDevicesNeedSelection(onlineSerials = onlineSerials)
        }
    }

    /**
     * Validates an ADB serial across all 4 supported connection paths:
     * - USB / Emulator (`R3CN1234567`, `emulator-5554`)
     * - Wi-Fi TCP/IP IPv4 (`192.168.1.50:5555`)
     * - Wi-Fi TCP/IP IPv6 (`[fe80::1]:5555`)
     * - Wireless Debugging mDNS TLS (`adb-R3CN1234-AbCdEf._adb-tls-connect._tcp`)
     *
     * A 1:1 port of Rust `arb_core::device::validate_adb_serial` so the GUI never accepts a
     * serial that `arbd` / the CLI reject: ASCII alphanumerics and `. _ - : [ ] %` only, at most
     * 255 bytes, no leading `-`, no `..`, and at most one leading `[host]` (optionally with a
     * single `%zone` id) optionally followed by a numeric `:port`.
     */
    fun isValidAdbSerial(serial: String): Boolean {
        if (serial.isEmpty() || serial.length > MAX_ADB_SERIAL_LEN) return false
        if (serial.startsWith("-") || serial.contains("..")) return false
        val allowed = serial.all { ch ->
            ch in 'a'..'z' || ch in 'A'..'Z' || ch in '0'..'9' || ch in ALLOWED_SERIAL_PUNCTUATION
        }
        if (!allowed) return false
        val openCount = serial.count { it == '[' }
        val closeCount = serial.count { it == ']' }
        if (openCount != closeCount || openCount > 1) return false
        if (openCount == 1) {
            if (!serial.startsWith("[")) return false
            val rest = serial.substring(1)
            val closeIdx = rest.indexOf(']')
            if (closeIdx < 0) return false
            val inside = rest.substring(0, closeIdx)
            val after = rest.substring(closeIdx + 1)
            if (inside.isEmpty()) return false
            val zoneIdx = inside.indexOf('%')
            if (zoneIdx >= 0) {
                val host = inside.substring(0, zoneIdx)
                val zone = inside.substring(zoneIdx + 1)
                if (host.isEmpty() || zone.isEmpty() || zone.contains('%')) return false
            }
            if (after.contains('%')) return false
            if (after.isNotEmpty()) {
                if (!after.startsWith(":")) return false
                val port = after.substring(1)
                if (port.isEmpty() || !port.all { it in '0'..'9' }) return false
            }
        } else if (serial.contains('%')) {
            return false
        }
        return true
    }

    private val ALLOWED_SERIAL_PUNCTUATION = charArrayOf('.', '_', '-', ':', '[', ']', '%')
}
