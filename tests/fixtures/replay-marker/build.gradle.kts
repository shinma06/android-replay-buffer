plugins {
    id("com.android.application") version "9.1.1" apply false
}

val source = providers.exec {
    commandLine("git", "rev-parse", "HEAD")
}.standardOutput.asText.get().trim()
val dirty = providers.exec {
    commandLine("git", "status", "--porcelain")
}.standardOutput.asText.get().isNotBlank()
extra["fixtureSource"] = source + if (dirty) "-dirty" else ""
