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
    private static final String FOUNDATION_SCROLLBAR_DESCRIPTOR =
            "(Landroidx/compose/foundation/v2/ScrollbarAdapter;Landroidx/compose/ui/Modifier;Z"
                    + "Landroidx/compose/foundation/ScrollbarStyle;"
                    + "Landroidx/compose/foundation/interaction/MutableInteractionSource;"
                    + "Landroidx/compose/runtime/Composer;II)V";
    private static final String FOUNDATION_ADAPTER_DESCRIPTOR =
            "(Landroidx/compose/foundation/ScrollState;Landroidx/compose/runtime/Composer;I)"
                    + "Landroidx/compose/foundation/v2/ScrollbarAdapter;";
    private static final String JEWEL_OPAQUE_THUMB_GETTER = "getThumbOpaqueBackground-0d7_KjU";

    @Test
    void scrollableViewsUseTheSharedScrollbarAndDoNotLinkTheVersionBoundContainer() throws IOException {
        String toolWindow = classFileText(ReplayToolWindowFactoryKt.class);
        String settings = classFileText(ReplaySettingsConfigurableKt.class);
        String scrollableColumn = classFileText(ScrollableColumnKt.class);

        assertTrue(toolWindow.contains("ScrollableColumn"));
        assertTrue(settings.contains("ScrollableColumn"));
        assertTrue(scrollableColumn.contains("verticalScroll"));
        assertFalse(toolWindow.contains("VerticallyScrollableContainer"));
        assertFalse(settings.contains("VerticallyScrollableContainer"));
        assertFalse(scrollableColumn.contains("VerticallyScrollableContainer"));
        assertFalse(scrollableColumn.contains("org/jetbrains/jewel/ui/component/ScrollbarKt"));
    }

    @Test
    void compiledScrollbarReferencesRabbitOneCompatibleFoundationDescriptors() throws IOException {
        String scrollableColumn = classFileText(ScrollableColumnKt.class);

        assertTrue(scrollableColumn.contains("androidx/compose/foundation/Scrollbar_desktopKt"));
        assertTrue(scrollableColumn.contains("VerticalScrollbar"));
        assertTrue(scrollableColumn.contains(FOUNDATION_SCROLLBAR_DESCRIPTOR));
        assertTrue(scrollableColumn.contains("rememberScrollbarAdapter2"));
        assertTrue(scrollableColumn.contains("androidx/compose/foundation/v2/ScrollbarAdapter"));
        assertTrue(scrollableColumn.contains(FOUNDATION_ADAPTER_DESCRIPTOR));
        assertTrue(scrollableColumn.contains("ScrollbarVisibility$AlwaysVisible"));
        assertTrue(scrollableColumn.contains("getLingerDuration-UwyO8pc"));
        assertTrue(scrollableColumn.contains("getThumbColorAnimationDuration-UwyO8pc"));
        assertTrue(scrollableColumn.contains(JEWEL_OPAQUE_THUMB_GETTER));
    }

    @Test
    void contentAndScrollbarShareOneScrollStateAndKeepTheRightSideThumbInteractive() throws IOException {
        String source = Files.readString(Path.of("src/main/kotlin/io/github/shinma06/replaybuffer/ui/ScrollableColumn.kt"));

        assertTrue(source.contains("val scrollState = rememberScrollState()"));
        assertTrue(source.contains("verticalScroll(scrollState)"));
        assertTrue(source.contains("rememberScrollbarAdapter(scrollState)"));
        assertTrue(source.contains("Modifier.align(Alignment.CenterEnd).fillMaxHeight()"));
        assertTrue(source.contains("reverseLayout = false"));
        assertTrue(source.contains("colors.thumbOpaqueBackground"));
        assertTrue(source.contains("colors.thumbBackgroundActive"));
        assertTrue(source.contains("lingerDuration.inWholeMilliseconds"));
        assertTrue(source.contains("thumbColorAnimationDuration.inWholeMilliseconds"));
    }

    private static String classFileText(Class<?> type) throws IOException {
        try (var bytecode = type.getResourceAsStream(type.getSimpleName() + ".class")) {
            assertNotNull(bytecode, type.getName());
            return new String(bytecode.readAllBytes(), StandardCharsets.ISO_8859_1);
        }
    }
}
