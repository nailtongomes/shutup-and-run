#!/usr/bin/env bash
# The variables cargo needs to build for Android with the NDK: the clang of
# API 24 (Android 7, Termux's minimum) as the C compiler and the linker, for
# aarch64 (the release's asset) and x86_64 (tests/internet/termux.sh).
# Prints KEY=VALUE lines:
#
#   bash scripts/android-ndk.sh >> "$GITHUB_ENV"           # CI
#   export $(bash scripts/android-ndk.sh ~/android-ndk-r27c) # by hand
#
# The NDK is the argument, else ANDROID_NDK_LATEST_HOME or ANDROID_NDK_HOME
# (GitHub's Ubuntu runners have one).
set -euo pipefail

ndk=${1:-${ANDROID_NDK_LATEST_HOME:-${ANDROID_NDK_HOME:-}}}
bin=$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin
if [[ -z "$ndk" || ! -x "$bin/aarch64-linux-android24-clang" ]]; then
  echo "android-ndk.sh: no Android NDK at '${ndk}': give its folder" >&2
  exit 1
fi
for arch in aarch64 x86_64; do
  upper=$(tr '[:lower:]' '[:upper:]' <<<"$arch")
  echo "CC_${arch}_linux_android=$bin/${arch}-linux-android24-clang"
  echo "AR_${arch}_linux_android=$bin/llvm-ar"
  echo "CARGO_TARGET_${upper}_LINUX_ANDROID_LINKER=$bin/${arch}-linux-android24-clang"
done
