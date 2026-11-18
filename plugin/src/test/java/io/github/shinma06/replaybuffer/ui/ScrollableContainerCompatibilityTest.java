package io.github.shinma06.replaybuffer.ui;

import io.github.shinma06.replaybuffer.settings.ReplaySettingsConfigurableKt;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ScrollableContainerCompatibilityTest {
    @Test
    void scrollableViewsUseTheSharedScrollbarAndDoNotLinkTheVersionBoundContainer() throws IOException {
        String toolWindow = classFileText(ReplayToolWindowFactoryKt.class);
        String settings = classFileText(ReplaySettingsConfigurableKt.class);
        String scrollableColumn = classFileText(ScrollableColumnKt.class);

        assertTrue(toolWindow.contains("ScrollableColumn"));
        assertTrue(settings.contains("ScrollableColumn"));
        assertTrue(scrollableColumn.contains("VerticalScrollbar"));
        assertTrue(scrollableColumn.contains("verticalScroll"));
        assertFalse(toolWindow.contains("VerticallyScrollableContainer"));
        assertFalse(settings.contains("VerticallyScrollableContainer"));
        assertFalse(scrollableColumn.contains("VerticallyScrollableContainer"));
    }

    @Test
    void contentAndScrollbarShareOneScrollStateAndKeepTheRightSideThumbInteractive() throws IOException {
        String source = Files.readString(Path.of("src/main/kotlin/io/github/shinma06/replaybuffer/ui/ScrollableColumn.kt"));

        assertTrue(source.contains("val scrollState = rememberScrollState()"));
        assertTrue(source.contains("verticalScroll(scrollState)"));
        assertTrue(source.contains("scrollState = scrollState"));
        assertTrue(source.contains("Modifier.align(Alignment.CenterEnd).fillMaxHeight()"));
        assertTrue(source.contains("enabled = true"));
        assertTrue(source.contains("keepVisible = keepVisible"));
    }

    private static String classFileText(Class<?> type) throws IOException {
        try (var bytecode = type.getResourceAsStream(type.getSimpleName() + ".class")) {
            assertNotNull(bytecode, type.getName());
            return new String(bytecode.readAllBytes(), StandardCharsets.ISO_8859_1);
        }
    }
}
