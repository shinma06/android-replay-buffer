#!/usr/bin/env bash
# Prepares the pinned Android SDK that device-agent/build.sh compiles against, for CI runners:
# android-37.2 `android.jar` (downloaded and SHA-256 verified) plus the runner's pre-installed
# build-tools (for d8). Prints the SDK directory and, under GitHub Actions, exports ANDROID_HOME
# through $GITHUB_ENV. Used by .github/workflows/{ci,release,release-gate}.yml.
set -euo pipefail

PLATFORM_ZIP_URL="https://dl.google.com/android/repository/platform-37.2_r01.zip"
PLATFORM_ZIP_SHA256="916410633df92a98da56aa7c87a1955f865f4d3b888373660794ef8173f72972"
ANDROID_JAR_SHA256="1db2419dbd45457f83b90ef8a7ef41e118174782c9a5c2185b55899df022767f"

runner_sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/usr/local/lib/android/sdk}}"
sdk_dir="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/replay-android-sdk.XXXXXX")"
archive="$sdk_dir/platform-37.2_r01.zip"

curl --fail --location --max-time 120 --output "$archive" "$PLATFORM_ZIP_URL"
echo "$PLATFORM_ZIP_SHA256  $archive" | sha256sum -c -
mkdir -p "$sdk_dir/platforms/android-37.2"
jar="$sdk_dir/platforms/android-37.2/android.jar"
unzip -p "$archive" android-37.2/android.jar > "$jar"
echo "$ANDROID_JAR_SHA256  $jar" | sha256sum -c -
rm -f "$archive"

# d8 は runner 既設の build-tools を借りる。
ln -s "$runner_sdk/build-tools" "$sdk_dir/build-tools"

if [[ -n "${GITHUB_ENV:-}" ]]; then
    echo "ANDROID_HOME=$sdk_dir" >> "$GITHUB_ENV"
fi
echo "$sdk_dir"
