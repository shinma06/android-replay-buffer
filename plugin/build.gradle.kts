import java.security.MessageDigest

plugins {
    kotlin("jvm") version "2.4.20"
    id("org.jetbrains.intellij.platform") version "2.19.0"
}

fun hash(file: java.io.File): String {
    val digest = MessageDigest.getInstance("SHA-256")
    file.inputStream().use { input ->
        val bytes = ByteArray(8192)
        while (true) { val n = input.read(bytes); if (n < 0) break; digest.update(bytes, 0, n) }
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
}

group = "io.github.shinma06.replaybuffer"
val sourceCommit = providers.exec { commandLine("git", "rev-parse", "HEAD") }.standardOutput.asText.get().trim()
val sourceDirty = providers.exec {
    commandLine("git", "status", "--porcelain", "--untracked-files=normal")
}.standardOutput.asText.get().isNotBlank()
version = "0.1.0-dev.$sourceCommit" + if (sourceDirty) "-dirty" else ""

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
    testImplementation("org.junit.jupiter:junit-jupiter:5.13.4")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher:1.13.4")
}

tasks.test {
    useJUnitPlatform()
    testLogging {
        exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL
        showStackTraces = false
    }
}

kotlin {
    jvmToolchain(25)
}

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
val clockLibrary = configurations.create("clockLibrary") { isTransitive = false }
dependencies {
    add(clockCompiler.name, "com.android.tools:r8:8.10.24")
    add(clockLibrary.name, "com.google.android:android:4.1.1.4")
}
val verifyCaptureInputs = tasks.register("verifyCaptureInputs") {
    val runtime = configurations.named("runtimeClasspath")
    inputs.files(clockCompiler, clockLibrary, runtime)
    inputs.file("src/main/resources/replay/deps/scrcpy-server-v4.0")
    doLast {
        val jcodec = runtime.get().files.single { it.name == "jcodec-0.2.5.jar" }
        check(hash(jcodec) == "890329dad124e8b739c1d6602a59a53c8a474daddff265c2561e21c498496c81")
        check(hash(clockCompiler.singleFile) == "ba8ec8958c4cf8d80168364f50c06742c7ef9313aae26eac0b9d28c309e04345")
        check(hash(clockLibrary.singleFile) == "84072541cbb711eff89f7277100ff854929a446dba7ceb1b195c340e0b4fd3cb")
        val server = file("src/main/resources/replay/deps/scrcpy-server-v4.0")
        check(hash(server) == "84924bd564a1eb6089c872c7521f968058977f91f5ff02514a8c74aff3210f3a")
    }
}
val compileClock = tasks.register<JavaCompile>("compileClock") {
    source = fileTree("src/clock/java") { include("**/*.java") }
    classpath = files()
    destinationDirectory.set(layout.buildDirectory.dir("clock/classes"))
    options.release.set(8)
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
    inputs.property("sourceCommit", sourceCommit)
    inputs.property("sourceDirty", sourceDirty)
    val output = layout.buildDirectory.file("clock/resource/replay/deps/identity.properties")
    outputs.file(output)
    doLast {
        val jcodec = runtime.get().files.single { it.name == "jcodec-0.2.5.jar" }
        val server = file("src/main/resources/replay/deps/scrcpy-server-v4.0")
        output.get().asFile.writeText("source=$sourceCommit\ndirty=$sourceDirty\nversion=${project.version}\nserver=${hash(server)}\nclock=${hash(clockJar.get().archiveFile.get().asFile)}\njcodec=${hash(jcodec)}\nr8=${hash(clockCompiler.singleFile)}\nandroid_api=${hash(clockLibrary.singleFile)}\n")
    }
}
tasks.processResources { dependsOn(captureIdentity) }

// Deterministic helper/library/plugin archives: same clean source + SDK/toolchain yields the same bytes.
tasks.withType<AbstractArchiveTask>().configureEach {
    isPreserveFileTimestamps = false
    isReproducibleFileOrder = true
}
