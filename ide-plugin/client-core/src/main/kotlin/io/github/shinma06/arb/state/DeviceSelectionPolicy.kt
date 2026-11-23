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
    private const val MAX_ADB_SERIAL_LEN: Int = 256

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
     * Strictly rejects empty strings, leading `-`, `..` traversal, whitespace, control characters,
     * and shell metacharacters.
     */
    fun isValidAdbSerial(serial: String): Boolean {
        if (serial.isEmpty() || serial.length > MAX_ADB_SERIAL_LEN) return false
        if (serial.startsWith("-") || serial.contains("..")) return false
        return serial.all { ch ->
            ch.isLetterOrDigit() || ch in charArrayOf('-', '_', '.', ':', '[', ']', '%')
        }
    }
}
