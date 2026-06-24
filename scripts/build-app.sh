#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dist="$repo_root/dist"
app_root="$dist/CCCodexProxy.app"
contents="$app_root/Contents"
macos="$contents/MacOS"
helpers="$contents/Helpers"
resources="$contents/Resources"

rm -rf "$app_root"
mkdir -p "$macos" "$helpers" "$resources"

cargo build --release -p cc-codex-proxy
swift build --package-path "$repo_root/macos/CCCodexProxy" -c release

if [[ -x "$repo_root/.build/release/CCCodexProxy" ]]; then
  install -m 755 "$repo_root/.build/release/CCCodexProxy" "$macos/CCCodexProxy"
else
  install -m 755 "$repo_root/macos/CCCodexProxy/.build/release/CCCodexProxy" "$macos/CCCodexProxy"
fi
install -m 755 "$repo_root/target/release/cc-codex-proxy" "$helpers/cc-codex-proxy"
cp "$repo_root/macos/CCCodexProxy/Info.plist" "$contents/Info.plist"

codesign --force --deep --sign - "$app_root"
ditto -c -k --keepParent "$app_root" "$dist/CCCodexProxy.zip"
echo "$dist/CCCodexProxy.zip"
