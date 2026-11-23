import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile

plugins {
    kotlin("jvm") version "2.4.21"
    kotlin("plugin.compose") version "2.4.21"
    id("org.jetbrains.intellij.platform") version "2.19.0"
}

group = "io.github.shinma06.arb"
val devBaseVersion = providers.gradleProperty("devBaseVersion").orElse("2.0.0").get()
version = providers.gradleProperty("pluginVersion").orNull ?: "$devBaseVersion-SNAPSHOT"

repositories {
    mavenCentral()
    google()
    intellijPlatform {
        defaultRepositories()
    }
}

dependencies {
    implementation(project(":client-core"))

    intellijPlatform {
        if (providers.gradleProperty("useLocalPlatform").orNull == "true") {
            local(providers.gradleProperty("platformPath"))
        } else {
            androidStudio("2026.2.2.8")
        }
        bundledPlugin("org.jetbrains.android")
        bundledPlugin("com.intellij.java")
        // Compose/Jewel bundled with the IDE (intellij.platform.compose and intellij.platform.jewel.*).
        @Suppress("UnstableApiUsage")
        composeUI()
    }

    testImplementation(kotlin("test-junit5"))
    testImplementation("org.junit.jupiter:junit-jupiter:5.13.4")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher:1.13.4")
}

java {
    sourceCompatibility = JavaVersion.VERSION_25
    targetCompatibility = JavaVersion.VERSION_25
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(25)
}

tasks.withType<KotlinJvmCompile>().configureEach {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_25)
        languageVersion.set(KotlinVersion.KOTLIN_2_4)
        apiVersion.set(KotlinVersion.KOTLIN_2_4)
    }
}

intellijPlatform {
    pluginConfiguration {
        name = "Android Replay Buffer (v2)"
        version = project.version.toString()
        ideaVersion {
            sinceBuild = "262.9437.185"
            untilBuild = "262.*"
        }
    }
}

tasks.buildSearchableOptions {
    enabled = false
}

// Bundle native Rust binaries (`native/<os>-<arch>/{arbd,arb-cli,arb-tray}`) and Android
// `device-agent/*.dex.jar` into the plugin sandbox / ZIP when available so Android Studio users
// can install one single `arb-ide-plugin-*.zip` with zero terminal setup.
fun detectHostNativeSlug(): String {
    val osName = System.getProperty("os.name").lowercase()
    val osSlug = when {
        osName.contains("mac") || osName.contains("darwin") -> "macos"
        osName.contains("win") -> "windows"
        else -> "linux"
    }
    val archRaw = System.getProperty("os.arch").lowercase()
    val archSlug = when (archRaw) {
        "aarch64", "arm64" -> "arm64"
        "amd64", "x86_64", "x64" -> "x86_64"
        else -> archRaw
    }
    return "$osSlug-$archSlug"
}

val arbNativeSlug = providers.gradleProperty("arbNativeSlug").orElse(detectHostNativeSlug()).get()
val arbExeExt = if (arbNativeSlug.startsWith("windows-")) ".exe" else ""
val arbNativeDir = providers.gradleProperty("arbNativeDir").orNull?.let { file(it) }
    ?: listOf(
        file("../core/target/release"),
        file("../core/target/debug"),
    ).firstOrNull { dir -> dir.resolve("arbd$arbExeExt").isFile }
val arbDeviceAgentDir = providers.gradleProperty("arbDeviceAgentDir").orNull?.let { file(it) }
    ?: file("../device-agent/build").takeIf { it.isDirectory }

tasks.prepareSandbox {
    val nativeSrc = arbNativeDir
    if (nativeSrc != null && nativeSrc.isDirectory) {
        from(nativeSrc) {
            include("arbd$arbExeExt", "arb-cli$arbExeExt", "arb-tray$arbExeExt")
            into("${pluginName.get()}/native/$arbNativeSlug")
            filePermissions {
                unix("rwxr-xr-x")
            }
        }
    }
    val agentSrc = arbDeviceAgentDir
    if (agentSrc != null && agentSrc.isDirectory) {
        from(agentSrc) {
            include("arb-companion.dex.jar", "replay-scrcpy-audio.dex.jar")
            into("${pluginName.get()}/device-agent")
        }
    }
}

tasks.buildPlugin {
    eachFile {
        if (relativePath.pathString.contains("/native/")) {
            permissions {
                unix("rwxr-xr-x")
            }
        }
    }
}

tasks.test {
    useJUnitPlatform()
    systemProperty("java.awt.headless", "true")
    systemProperty("arb.repo.root", projectDir.parentFile.absolutePath)
    testLogging {
        events("passed", "skipped", "failed")
        exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL
    }
}
