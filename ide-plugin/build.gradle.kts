import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile

plugins {
    kotlin("jvm") version "2.4.21"
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

tasks.test {
    useJUnitPlatform()
    systemProperty("java.awt.headless", "true")
    systemProperty("arb.repo.root", projectDir.parentFile.absolutePath)
    testLogging {
        events("passed", "skipped", "failed")
        exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL
    }
}
