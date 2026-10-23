#!/usr/bin/env bash
# Packages a release build of Peek into dist/.
#
#   scripts/package.sh <target-triple> <asset-name>
#
# Expects target/<target-triple>/release/peek[.exe] to exist. Produces:
#   windows  dist/<asset-name>.zip      peek.exe, README, licenses
#   linux    dist/<asset-name>.tar.gz   peek, launcher entry, icon, install.sh
#   macos    dist/<asset-name>.zip      Peek.app (ad-hoc signed)
set -euo pipefail

target=$1
name=$2
root=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
bin="$root/target/$target/release/peek"
dist="$root/dist"
stage="$dist/$name"

rm -rf "$stage"
mkdir -p "$stage"
docs=("$root/README.md" "$root/LICENSE-MIT" "$root/LICENSE-APACHE")

case "$target" in
  *windows*)
    cp "$bin.exe" "${docs[@]}" "$stage/"
    (cd "$dist" && rm -f "$name.zip" && 7z a -tzip -bso0 "$name.zip" "$name")
    ;;
  *linux*)
    cp "$bin" "${docs[@]}" "$stage/"
    cp "$root/packaging/linux/peek.desktop" "$root/packaging/linux/install.sh" "$stage/"
    cp "$root/crates/peek-app/assets/peek.svg" "$stage/"
    chmod 755 "$stage/peek" "$stage/install.sh"
    tar -C "$dist" -czf "$dist/$name.tar.gz" "$name"
    ;;
  *apple*)
    app="$stage/Peek.app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp "$bin" "$app/Contents/MacOS/peek"
    sed "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
    iconset="$dist/peek.iconset"
    rm -rf "$iconset" && mkdir -p "$iconset"
    src="$root/packaging/macos/icon-1024.png"
    for s in 16 32 128 256 512; do
      sips -z $s $s "$src" --out "$iconset/icon_${s}x${s}.png" >/dev/null
      sips -z $((s * 2)) $((s * 2)) "$src" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
    done
    iconutil -c icns "$iconset" -o "$app/Contents/Resources/peek.icns"
    rm -rf "$iconset"
    cp "${docs[@]}" "$stage/"
    # Not notarized: an ad-hoc signature keeps the bundle intact on Apple silicon.
    codesign --force --deep --sign - "$app"
    (cd "$dist" && rm -f "$name.zip" && ditto -c -k --keepParent "$name" "$name.zip")
    ;;
  *)
    echo "unknown target: $target" >&2
    exit 1
    ;;
esac

rm -rf "$stage"
ls -l "$dist"
