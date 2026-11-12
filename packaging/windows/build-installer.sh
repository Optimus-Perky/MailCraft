#!/bin/bash
# Packages an already-built MailCraft.exe as the MailCraft.exe installer (an MSI inside a WiX
# Burn bundle).
#
#   packaging/windows/build-installer.sh <dir holding mailcraft.exe and WebView2Loader.dll> [out dir]
#
# Build the program with MAILCRAFT_VERSION set to what `packaging/windows/build-installer.sh
# --version` prints: Windows Installer replaces a program file only with a newer version, so
# the exe must carry the installer's version. This refuses an exe that does not.
#
# WiX needs Windows (it builds the database through msi.dll), so the files go over SSH to a
# host with the WiX 3.14 binaries in C:\wix314. Set WIX_HOST / WIX_USER / WIX_KEY to move it.
set -euo pipefail
cd "$(dirname "$0")/../.."
# The number Windows orders builds by, which must keep rising: the commit count.
BASE=$(sed -n 's/^version = "\([0-9]*\.[0-9]*\)\..*/\1/p' Cargo.toml | head -1)
VERSION="$BASE.$(git rev-list --count HEAD)"
if [ "${1:-}" = "--version" ]; then
    echo "$VERSION"
    exit 0
fi
BIN=${1:?usage: build-installer.sh <bin dir> [out dir]   (or --version)}
OUT=${2:-dist}
WIX_HOST=${WIX_HOST:-wix-build-host}
WIX_USER=${WIX_USER:-Administrator}
WIX_KEY=${WIX_KEY:-$HOME/.ssh/id_ed25519}
SSH="ssh -i $WIX_KEY -o StrictHostKeyChecking=no -o BatchMode=yes $WIX_USER@$WIX_HOST"
SCP="scp -q -i $WIX_KEY -o StrictHostKeyChecking=no"

T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
cp "$BIN/mailcraft.exe" "$T/MailCraft.exe"
cp "$BIN/WebView2Loader.dll" assets/app-icon/mailcraft.ico assets/app-icon/mailcraft-256.png \
   LICENSE-MIT LICENSE-APACHE NOTICE packaging/windows/mailcraft.wxs packaging/windows/bundle.wxs \
   packaging/windows/wixbuild.ps1 "$T/"
# The licence page of the installer: LICENSE-MIT as plain RTF.
{
  printf '{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Segoe UI;}}\\f0\\fs18 '
  printf 'MailCraft is free software, licensed under either the MIT licence or the Apache License 2.0, at your option. Both are installed beside the program.\\par\\par '
  sed -e 's/\\/\\\\/g; s/{/\\{/g; s/}/\\}/g; s/$/\\par/' LICENSE-MIT
  printf '}'
} > "$T/notice.rtf"

$SSH "powershell -NoProfile -Command \"Remove-Item C:\mailcraftbuild -Recurse -Force -EA 0; New-Item -ItemType Directory -Path C:\mailcraftbuild -Force | Out-Null\"" >/dev/null
$SCP "$T"/* "$WIX_USER@$WIX_HOST:C:/mailcraftbuild/"
$SSH "powershell -NoProfile -ExecutionPolicy Bypass -File C:\\mailcraftbuild\\wixbuild.ps1 -Version $VERSION"
mkdir -p "$OUT"
$SCP "$WIX_USER@$WIX_HOST:C:/mailcraftbuild/MailCraft.exe" "$OUT/MailCraft.exe"
( cd "$OUT" && sha256sum MailCraft.exe | tee MailCraft.exe.sha256 )
