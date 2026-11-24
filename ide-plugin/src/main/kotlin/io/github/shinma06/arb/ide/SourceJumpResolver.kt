package io.github.shinma06.arb.ide

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.fileEditor.OpenFileDescriptor
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Computable
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.psi.JavaPsiFacade
import com.intellij.psi.search.FilenameIndex
import com.intellij.psi.search.GlobalSearchScope
import com.intellij.psi.search.PsiShortNamesCache
import io.github.shinma06.arb.state.SourceJumpTarget

/**
 * Abstraction for PC-side IDE source navigation from Activity names, crash classes,
 * and stack-trace frames. Purely PC-side (zero device load) and cleanly testable.
 */
interface SourceJumpResolver {
    /**
     * Resolves [target] against the open IDE project and navigates to the matched file/line.
     * Returns `true` when navigation succeeded, `false` if no matching source exists in the project.
     */
    fun resolveAndNavigate(target: SourceJumpTarget): Boolean

    companion object {
        val NO_OP: SourceJumpResolver = object : SourceJumpResolver {
            override fun resolveAndNavigate(target: SourceJumpTarget): Boolean = false
        }
    }
}

/**
 * Production Android Studio / IntelliJ Platform [SourceJumpResolver] backed by
 * [JavaPsiFacade], [FilenameIndex], and [PsiShortNamesCache].
 */
class IntelliJSourceJumpResolver(
    private val project: Project,
) : SourceJumpResolver {
    private val logger = Logger.getInstance(IntelliJSourceJumpResolver::class.java)

    override fun resolveAndNavigate(target: SourceJumpTarget): Boolean {
        if (project.isDisposed || !target.isResolvable) return false

        val resolved = runCatching {
            ApplicationManager.getApplication().runReadAction(
                Computable<Pair<VirtualFile, Int>?> { resolveInReadAction(target) },
            )
        }.onFailure { err ->
            logger.debug("ARB PSI source jump failed for $target", err)
        }.getOrNull() ?: return false

        val (vf, zeroBasedLine) = resolved
        val openTask = Runnable {
            if (!project.isDisposed && vf.isValid) {
                OpenFileDescriptor(project, vf, zeroBasedLine.coerceAtLeast(0), 0).navigate(true)
            }
        }
        val app = ApplicationManager.getApplication()
        if (app.isDispatchThread) {
            openTask.run()
        } else {
            app.invokeLater(openTask)
        }
        return true
    }

    private fun resolveInReadAction(target: SourceJumpTarget): Pair<VirtualFile, Int>? {
        val projectScope = GlobalSearchScope.projectScope(project)
        val allScope = GlobalSearchScope.allScope(project)
        val zeroBasedLine = ((target.lineNumber ?: 1) - 1).coerceAtLeast(0)

        // 1. Exact FQN lookup via JavaPsiFacade (covers both Kotlin & Java compiled/source classes)
        target.classFqn?.takeIf { it.isNotBlank() }?.let { fqn ->
            resolveViaJavaPsiFacade(fqn, projectScope, allScope)?.let { vf ->
                return vf to zeroBasedLine
            }
        }

        // 2. Exact filename lookup via FilenameIndex (works even without Java plugin & handles top-level .kt files)
        target.fileName?.takeIf { it.isNotBlank() }?.let { fileName ->
            val matches = FilenameIndex.getVirtualFilesByName(fileName, projectScope)
            val best = chooseBestFileMatch(matches, target.classFqn)
            if (best != null) {
                return best to zeroBasedLine
            }
        }

        // 3. Short class name lookup via PsiShortNamesCache (covers Activity short names)
        target.shortClassName?.takeIf { it.isNotBlank() }?.let { shortName ->
            resolveViaShortNamesCache(shortName, projectScope)?.let { vf ->
                return vf to zeroBasedLine
            }
        }

        return null
    }

    private fun resolveViaJavaPsiFacade(
        fqn: String,
        projectScope: GlobalSearchScope,
        allScope: GlobalSearchScope,
    ): VirtualFile? = try {
        val facade = JavaPsiFacade.getInstance(project)
        val psiClass = facade.findClass(fqn, projectScope) ?: facade.findClass(fqn, allScope)
        psiClass?.navigationElement?.containingFile?.virtualFile
    } catch (_: LinkageError) {
        null
    }

    private fun resolveViaShortNamesCache(
        shortName: String,
        projectScope: GlobalSearchScope,
    ): VirtualFile? = try {
        val cache = PsiShortNamesCache.getInstance(project)
        cache.getClassesByName(shortName, projectScope)
            .firstOrNull()
            ?.navigationElement
            ?.containingFile
            ?.virtualFile
    } catch (_: LinkageError) {
        null
    }

    private fun chooseBestFileMatch(candidates: Collection<VirtualFile>, classFqn: String?): VirtualFile? {
        if (candidates.isEmpty()) return null
        if (candidates.size == 1 || classFqn.isNullOrBlank()) return candidates.first()
        val pkgPath = classFqn.substringBeforeLast('.', "").replace('.', '/')
        return candidates.firstOrNull { vf -> pkgPath.isNotEmpty() && vf.path.contains(pkgPath) }
            ?: candidates.first()
    }
}
