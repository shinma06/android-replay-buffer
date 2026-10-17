import java.security.MessageDigest
import java.util.HexFormat
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile

plugins {
    kotlin("jvm") version "2.4.21"
    id("org.jetbrains.intellij.platform") version "2.19.0"
}

fun hash(file: java.io.File): String {
    val digest = MessageDigest.getInstance("SHA-256")
    file.inputStream().use { input ->
        val bytes = ByteArray(8192)
        while (true) { val n = input.read(bytes); if (n < 0) break; digest.update(bytes, 0, n) }
    }
    return HexFormat.of().formatHex(digest.digest())
}

group = "io.github.shinma06.replaybuffer"
val sourceCommit = providers.exec { commandLine("git", "rev-parse", "HEAD") }.standardOutput.asText.get().trim()
val sourceDirty = providers.exec {
    commandLine("git", "status", "--porcelain", "--untracked-files=normal")
}.standardOutput.asText.get().isNotBlank()
version = providers.gradleProperty("pluginVersion").orNull?.also {
    require(Regex("(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)").matches(it)) {
        "pluginVersion must be a formal major.minor.patch version (no prefix, suffix, or leading zeroes)"
    }
    check(!sourceDirty) { "A formal pluginVersion requires a clean committed source tree" }
} ?: ("0.2.0-dev.$sourceCommit" + if (sourceDirty) "-dirty" else "")

repositories {
    mavenCentral()
    google()
    intellijPlatform { defaultRepositories() }
}

dependencies {
    intellijPlatform {
        if (providers.gradleProperty("useLocalPlatform").orNull == "true") {
            local(providers.gradleProperty("platformPath"))
        } else {
            androidStudio("2026.2.1.8")
        }
        bundledPlugin("org.jetbrains.android")
    }
    implementation("org.jcodec:jcodec:0.2.5")
    testImplementation(kotlin("test-junit5"))
    testImplementation(platform("org.junit:junit-bom:6.1.3"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.test {
    useJUnitPlatform()
    systemProperty("replay.expected.source", sourceCommit)
    systemProperty("replay.expected.dirty", sourceDirty)
    systemProperty("replay.expected.version", project.version.toString())
    testLogging {
        exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL
        showStackTraces = false
    }
}

kotlin {
    jvmToolchain(27)
    compilerOptions {
        languageVersion.set(KotlinVersion.KOTLIN_2_4)
        apiVersion.set(KotlinVersion.KOTLIN_2_4)
        freeCompilerArgs.add("-Xjdk-release=25")
    }
}

java {
    sourceCompatibility = JavaVersion.VERSION_25
    targetCompatibility = JavaVersion.VERSION_25
}
tasks.withType<JavaCompile>().configureEach { options.release.set(25) }
// Override the platform plugin's task convention, which otherwise derives the target from JDK 27.
tasks.withType<KotlinJvmCompile>().configureEach { compilerOptions.jvmTarget.set(JvmTarget.JVM_25) }

intellijPlatform {
    pluginConfiguration {
        name = "Android Replay Buffer"
        version = project.version.toString()
        ideaVersion {
            sinceBuild = "262.9437.185"
            untilBuild = "262.*"
        }
    }
}

val verifyBuildSdk = tasks.register("verifyBuildSdk") {
    group = "verification"
    doLast {
        val info = intellijPlatform.productInfo
        check(info.productCode == "AI" && info.buildNumber == "262.9437.185.2621.16467767") {
            "Expected Android Studio Rabbit 1 (AI-262.9437.185.2621.16467767), got ${info.productCode}-${info.buildNumber}"
        }
    }
}

tasks.matching {
    it.name in setOf("compileKotlin", "compileJava", "processResources", "prepareSandbox", "buildPlugin")
}.configureEach { dependsOn(verifyBuildSdk) }

tasks.buildSearchableOptions { enabled = false }

// Build-only D8: users need neither an Android build-tools installation nor a runtime download.
val clockCompiler = configurations.create("clockCompiler")
val clockLibrary = files(providers.environmentVariable("ANDROID_HOME").map { "$it/platforms/android-37.0/android.jar" })
dependencies {
    add(clockCompiler.name, "com.android.tools:r8:9.5.26")
}
val verifyCaptureInputs = tasks.register("verifyCaptureInputs") {
    val runtime = configurations.named("runtimeClasspath")
    inputs.files(clockCompiler, clockLibrary, runtime)
    inputs.file("src/main/resources/replay/deps/scrcpy-server-v5.0.1")
    doLast {
        val jcodec = runtime.get().files.single { it.name == "jcodec-0.2.5.jar" }
        check(hash(jcodec) == "890329dad124e8b739c1d6602a59a53c8a474daddff265c2561e21c498496c81")
        check(hash(clockCompiler.singleFile) == "3502f087388ce95bbf5254555927f09385dc2636873964d0bd53f3eaf4fb49ad")
        check(hash(clockLibrary.singleFile) == "bf1b4387cc7ca94fc6ef684f040d9d16fbf16248e181819f020736ea2053f177")
        val server = file("src/main/resources/replay/deps/scrcpy-server-v5.0.1")
        check(hash(server) == "764eb6f79811d5211fe9df341120882ba9994c7a61b897d7bf3fb662e53bc536")
    }
}
val compileClock = tasks.register<JavaCompile>("compileClock") {
    source = fileTree("src/clock/java") { include("**/*.java") }
    dependsOn(verifyCaptureInputs)
    classpath = clockLibrary
    destinationDirectory.set(layout.buildDirectory.dir("clock/classes"))
}
val clockClasses = tasks.register<Jar>("clockClasses") {
    dependsOn(compileClock)
    from(compileClock.flatMap { it.destinationDirectory })
    archiveFileName.set("clock-classes.jar")
    destinationDirectory.set(layout.buildDirectory.dir("clock"))
    isPreserveFileTimestamps = false
    isReproducibleFileOrder = true
}
val dexClock = tasks.register<JavaExec>("dexClock") {
    dependsOn(clockClasses, verifyCaptureInputs)
    classpath = clockCompiler
    inputs.files(clockLibrary)
    inputs.property("minimumApi", 24)
    mainClass.set("com.android.tools.r8.D8")
    inputs.file(clockClasses.flatMap { it.archiveFile })
    outputs.dir(layout.buildDirectory.dir("clock/dex"))
    doFirst {
        val output = layout.buildDirectory.dir("clock/dex").get().asFile
        output.mkdirs()
        args("--release", "--min-api", "24", "--lib", clockLibrary.singleFile.absolutePath,
            "--output", output.absolutePath, clockClasses.get().archiveFile.get().asFile.absolutePath)
    }
}
val clockJar = tasks.register<Jar>("clockJar") {
    dependsOn(dexClock)
    from(layout.buildDirectory.dir("clock/dex"))
    archiveFileName.set("replay-clock.jar")
    destinationDirectory.set(layout.buildDirectory.dir("clock/resource/replay/deps"))
    isPreserveFileTimestamps = false
    isReproducibleFileOrder = true
}
tasks.processResources {
    dependsOn(clockJar)
    from(layout.buildDirectory.dir("clock/resource"))
}

val captureIdentity = tasks.register("captureIdentity") {
    dependsOn(clockJar)
    val runtime = configurations.named("runtimeClasspath")
    inputs.files(clockJar.flatMap { it.archiveFile }, clockCompiler, clockLibrary, runtime)
    inputs.file("src/main/resources/replay/deps/scrcpy-server-v5.0.1")
    inputs.property("sourceCommit", sourceCommit)
    inputs.property("sourceDirty", sourceDirty)
    inputs.property("version", project.version.toString())
    val output = layout.buildDirectory.file("clock/resource/replay/deps/identity.properties")
    outputs.file(output)
    doLast {
        val jcodec = runtime.get().files.single { it.name == "jcodec-0.2.5.jar" }
        val server = file("src/main/resources/replay/deps/scrcpy-server-v5.0.1")
        output.get().asFile.writeText("source=$sourceCommit\ndirty=$sourceDirty\nversion=${project.version}\nserver=${hash(server)}\nclock=${hash(clockJar.get().archiveFile.get().asFile)}\njcodec=${hash(jcodec)}\nr8=${hash(clockCompiler.singleFile)}\nandroid_api=${hash(clockLibrary.singleFile)}\n")
    }
}
tasks.processResources { dependsOn(captureIdentity) }

// Deterministic helper/library/plugin archives: same clean source + SDK/toolchain yields the same bytes.
tasks.withType<AbstractArchiveTask>().configureEach {
    isPreserveFileTimestamps = false
    isReproducibleFileOrder = true
}
