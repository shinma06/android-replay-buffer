package io.github.shinma06.arb.ide

import com.intellij.testFramework.LightVirtualFile

/**
 * Lightweight virtual file representing an ARB or Legacy (RO) bundle tab in the IDE editor area.
 */
class ArbBundleVirtualFile(
    val bundleIdOrPath: String,
    val displayTitle: String,
    val isLegacyHint: Boolean = false,
) : LightVirtualFile("$displayTitle.arbstudio", ArbBundleFileType, "") {
    init {
        isWritable = false
    }

    override fun getPresentableName(): String =
        if (isLegacyHint) "$displayTitle [Legacy (RO)]" else displayTitle

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ArbBundleVirtualFile) return false
        return bundleIdOrPath == other.bundleIdOrPath
    }

    override fun hashCode(): Int = bundleIdOrPath.hashCode()
}
