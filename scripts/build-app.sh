#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dist="$repo_root/dist"
app_root="$dist/CCCodexProxy.app"
contents="$app_root/Contents"
macos="$contents/MacOS"
helpers="$contents/Helpers"
resources="$contents/Resources"
version="${CCP_VERSION:-}"
build_number="${CCP_BUILD_NUMBER:-${GITHUB_RUN_NUMBER:-1}}"

if [[ -z "$version" ]]; then
  version="$(grep -m1 '^version = ' "$repo_root/crates/cc-codex-proxy/Cargo.toml" | cut -d '"' -f2)"
fi

artifact_prefix="CCCodexProxy-$version-macOS"
zip_path="$dist/$artifact_prefix.zip"
dmg_path="$dist/$artifact_prefix.dmg"
latest_zip_path="$dist/CCCodexProxy-macOS.zip"
latest_dmg_path="$dist/CCCodexProxy-macOS.dmg"
checksums_path="$dist/SHA256SUMS"

rm -rf "$app_root" "$zip_path" "$dmg_path" "$latest_zip_path" "$latest_dmg_path" "$checksums_path"
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
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $build_number" "$contents/Info.plist"

codesign --force --sign - "$helpers/cc-codex-proxy"
codesign --force --deep --sign - "$app_root"

ditto -c -k --keepParent "$app_root" "$zip_path"
cp "$zip_path" "$latest_zip_path"
cp "$zip_path" "$dist/CCCodexProxy.zip"

dmg_stage="$(mktemp -d)"
trap 'rm -rf "$dmg_stage"' EXIT
cp -R "$app_root" "$dmg_stage/CCCodexProxy.app"
ln -s /Applications "$dmg_stage/Applications"
hdiutil create \
  -volname "CC Codex Proxy" \
  -srcfolder "$dmg_stage" \
  -ov \
  -format UDZO \
  "$dmg_path"
cp "$dmg_path" "$latest_dmg_path"

(
  cd "$dist"
  shasum -a 256 \
    "$(basename "$dmg_path")" \
    "$(basename "$zip_path")" \
    "$(basename "$latest_dmg_path")" \
    "$(basename "$latest_zip_path")" \
    > "$(basename "$checksums_path")"
)

printf '%s\n' "$dmg_path" "$zip_path" "$latest_dmg_path" "$latest_zip_path" "$checksums_path"
