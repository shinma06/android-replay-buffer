package privateacceptance.windowsmenu;
import com.intellij.openapi.actionSystem.AnActionEvent;
import com.intellij.openapi.project.DumbAwareAction;
public final class StartAction extends DumbAwareAction {
    @Override public void actionPerformed(AnActionEvent e) {
        if (e.getProject()!=null) e.getProject().getService(WindowsMenuPrototype.class).start();
    }
}
