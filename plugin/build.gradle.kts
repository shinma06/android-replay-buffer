plugins {
    kotlin("jvm") version "2.4.20"
    id("org.jetbrains.intellij.platform") version "2.19.0"
}

group = "io.github.shinma06.replaybuffer"
val sourceCommit = providers.exec { commandLine("git", "rev-parse", "HEAD") }.standardOutput.asText.get().trim()
val sourceDirty = providers.exec {
    commandLine("git", "status", "--porcelain", "--untracked-files=normal")
}.standardOutput.asText.get().isNotBlank()
version = "0.1.0-dev.$sourceCommit" + if (sourceDirty) "-dirty" else ""

repositories {
    mavenCentral()
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
    testImplementation(kotlin("test-junit5"))
    testImplementation("org.junit.jupiter:junit-jupiter:5.13.4")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher:1.13.4")
}

tasks.test { useJUnitPlatform() }

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
