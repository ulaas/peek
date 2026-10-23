#!/usr/bin/env bash
# Packages a release build of Peek into dist/.
#
#   scripts/package.sh <target-triple> <asset-name>
#
# Expects target/<target-triple>/release/peek[.exe] to exist. Produces a
# portable archive and an installer for the platform:
#   windows  <name>.zip     peek.exe, README, licenses
#            <name>.msi     installer (WiX Toolset v5: `wix` on PATH with WixToolset.UI.wixext)
#   linux    <name>.tar.gz  peek, launcher entry, icon, install.sh
#            <name>.deb     package for Debian and Ubuntu (dpkg-deb)
#            <name>.rpm     package for Fedora and openSUSE (rpmbuild)
#   macos    <name>.zip     Peek.app (ad-hoc signed)
#            <name>.dmg     disk image with Peek.app and an Applications link
set -euo pipefail

target=$1
name=$2
root=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
# Debian and RPM sort a pre-release before its release only with "~": 0.2.0~beta.1.
pkgversion=$(echo "$version" | tr - '~')
bin="$root/target/$target/release/peek"
dist="$root/dist"
stage="$dist/$name"
work="$dist/work"

rm -rf "$stage" "$work"
mkdir -p "$stage" "$work"
docs=("$root/README.md" "$root/LICENSE-MIT" "$root/LICENSE-APACHE")

case "$target" in
  *windows*)
    cp "$bin.exe" "${docs[@]}" "$stage/"
    (cd "$dist" && rm -f "$name.zip" && 7z a -tzip -bso0 "$name.zip" "$name")
    # MSI versions are numeric only: 0.2.0-beta.1 installs as 0.2.0.
    (cd "$root/packaging/windows" && wix build peek.wxs -arch x64 -ext WixToolset.UI.wixext \
      -d "Version=${version%%-*}" -d "BinDir=$(cygpath -w "$stage")" -o "$(cygpath -w "$dist/$name.msi")")
    rm -f "$dist/$name.wixpdb"
    ;;
  *linux*)
    cp "$bin" "${docs[@]}" "$stage/"
    cp "$root/packaging/linux/peek.desktop" "$root/packaging/linux/install.sh" "$stage/"
    cp "$root/crates/peek-app/assets/peek.svg" "$stage/"
    chmod 755 "$stage/peek" "$stage/install.sh"
    tar -C "$dist" -czf "$dist/$name.tar.gz" "$name"

    # One file tree for both packages.
    tree="$work/tree"
    install -Dm755 "$bin" "$tree/usr/bin/peek"
    install -Dm644 "$root/packaging/linux/peek.desktop" "$tree/usr/share/applications/peek.desktop"
    install -Dm644 "$root/crates/peek-app/assets/peek.svg" "$tree/usr/share/icons/hicolor/scalable/apps/peek.svg"
    install -Dm644 "$root/packaging/linux/peek.xml" "$tree/usr/share/mime/packages/peek.xml"
    install -Dm644 "$root/LICENSE-MIT" "$tree/usr/share/licenses/peek/LICENSE-MIT"
    install -Dm644 "$root/LICENSE-APACHE" "$tree/usr/share/licenses/peek/LICENSE-APACHE"
    echo "Shared libraries peek links against:"
    readelf -d "$bin" | grep NEEDED || true

    deb="$work/deb"
    cp -a "$tree" "$deb"
    mkdir -p "$deb/DEBIAN" "$deb/usr/share/doc/peek"
    cp "$root/LICENSE-MIT" "$deb/usr/share/doc/peek/copyright"
    size=$(du -sk "$deb/usr" | cut -f1)
    sed -e "s/@VERSION@/$pkgversion/" -e "s/@SIZE@/$size/" "$root/packaging/linux/deb-control" > "$deb/DEBIAN/control"
    dpkg-deb --root-owner-group --build "$deb" "$dist/$name.deb"

    rpmtop="$work/rpm"
    mkdir -p "$rpmtop"
    rpmbuild -bb --quiet --define "_topdir $rpmtop" --define "pkgver $pkgversion" --define "stage $tree" \
      "$root/packaging/linux/peek.spec"
    cp "$rpmtop"/RPMS/*/peek-*.rpm "$dist/$name.rpm"
    ;;
  *apple*)
    app="$stage/Peek.app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp "$bin" "$app/Contents/MacOS/peek"
    sed "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
    iconset="$work/peek.iconset"
    mkdir -p "$iconset"
    src="$root/packaging/macos/icon-1024.png"
    for s in 16 32 128 256 512; do
      sips -z $s $s "$src" --out "$iconset/icon_${s}x${s}.png" >/dev/null
      sips -z $((s * 2)) $((s * 2)) "$src" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
    done
    iconutil -c icns "$iconset" -o "$app/Contents/Resources/peek.icns"
    # Not notarized: an ad-hoc signature keeps the bundle intact on Apple silicon.
    codesign --force --deep --sign - "$app"

    dmg="$work/dmg"
    mkdir -p "$dmg"
    cp -R "$app" "$dmg/"
    ln -s /Applications "$dmg/Applications"
    hdiutil create -volname Peek -srcfolder "$dmg" -fs HFS+ -format UDZO -ov "$dist/$name.dmg"

    cp "${docs[@]}" "$stage/"
    (cd "$dist" && rm -f "$name.zip" && ditto -c -k --keepParent "$name" "$name.zip")
    ;;
  *)
    echo "unknown target: $target" >&2
    exit 1
    ;;
esac

rm -rf "$stage" "$work"
ls -l "$dist"
