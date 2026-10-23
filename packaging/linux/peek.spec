# Packs the prebuilt release binary; built by scripts/package.sh with
#   rpmbuild -bb --define "_topdir <dir>" --define "pkgver <v>" --define "stage <dir>"
%global debug_package %{nil}
%global _build_id_links none

Name:           peek
Version:        %{pkgver}
Release:        1
Summary:        Fast, quiet remote desktop client
License:        MIT OR Apache-2.0
URL:            https://ulaas.github.io/peek/
Recommends:     libxkbcommon, libxkbcommon-x11, libwayland-client, libX11, libXcursor, libXi, libXrandr, vulkan-loader

%description
Peek connects to Windows computers over RDP and to Linux desktops,
Raspberry Pis and Macs over VNC. It has tabs, a command palette, a
connection library with passwords in the system keychain, two-way
clipboard, remote audio and session recording.

%install
mkdir -p %{buildroot}
cp -a %{stage}/usr %{buildroot}/

%files
%{_bindir}/peek
%{_datadir}/applications/peek.desktop
%{_datadir}/icons/hicolor/scalable/apps/peek.svg
%{_datadir}/mime/packages/peek.xml
%license %{_datadir}/licenses/peek/LICENSE-MIT
%license %{_datadir}/licenses/peek/LICENSE-APACHE
