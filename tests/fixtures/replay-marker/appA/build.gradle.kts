plugins {
    id("com.android.application")
}

android {
    namespace = "io.github.shinma06.replaybuffer.fixture"
    compileSdk { version = release(37) { minorApiLevel = 0 } }
    buildToolsVersion = "36.0.0"
    defaultConfig {
        applicationId = "io.github.shinma06.replaybuffer.qa.appa"
        minSdk = 29
        targetSdk = 37
        versionCode = 1
        versionName = "qa." + rootProject.extra["fixtureSource"]
        buildConfigField("String", "FIXTURE_SOURCE", "\"${rootProject.extra["fixtureSource"]}\"")
        manifestPlaceholders["appLabel"] = "Replay QA A"
    }
    buildTypes.getByName("debug") { applicationIdSuffix = ".debug" }
    buildFeatures { buildConfig = true }
    sourceSets.getByName("main") {
        java.directories.add("../shared/src/main/java")
        manifest.srcFile("../shared/src/main/AndroidManifest.xml")
    }
    signingConfigs.getByName("debug") {
        storeFile = rootProject.file(".private/qa.keystore")
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
