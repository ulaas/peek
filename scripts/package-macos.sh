#!/usr/bin/env bash
# Builds Peek for Apple silicon and packages it, on a Mac, from a clean checkout:
#
#   scripts/package-macos.sh
#
# Produces dist/peek-macos-arm64.dmg (Peek.app and an Applications link) and
# dist/peek-macos-arm64.zip, the same files the release workflow publishes.
# Needs Rust (rustup), CMake (for the bundled Opus) and the Xcode command line tools.
#
# Signing is ad-hoc by default. To sign with a Developer ID and notarize:
#   MACOS_SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" \
#   MACOS_NOTARY_PROFILE=<profile saved with `xcrun notarytool store-credentials`> \
#   scripts/package-macos.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
target=aarch64-apple-darwin
name=${1:-peek-macos-arm64}

[ "$(uname -s)" = Darwin ] || { echo "run this on macOS" >&2; exit 1; }
command -v cmake >/dev/null || { echo "CMake is needed for the bundled Opus: brew install cmake" >&2; exit 1; }

# Opus from its vendored source, linked statically: the app needs no Homebrew libraries.
export OPUS_BUNDLED=1 OPUS_STATIC=1
# C code (Opus) built for the same oldest macOS as the Rust code and Info.plist.
export MACOSX_DEPLOYMENT_TARGET=11.0

cd "$root"
rustup target add "$target" >/dev/null
cargo build --release --locked -p peek --target "$target"
bash "$root/scripts/package.sh" "$target" "$name"
