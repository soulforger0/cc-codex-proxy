#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dist="$repo_root/dist"
version="${1:-${CCP_VERSION:-}}"
expected_arch="${2:-${CCP_RELEASE_ARCH:-arm64}}"

usage() {
  cat <<'EOF'
Usage:
  scripts/verify-release-packaging.sh <version> [expected_arch]

Verifies release artifacts, checksums, app binary architecture, release manifest
metadata, and Homebrew cask architecture requirements.
EOF
}

if [[ "${version:-}" == "-h" || "${version:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ -z "$version" ]]; then
  version="$(grep -m1 '^version = ' "$repo_root/crates/cc-codex-proxy/Cargo.toml" | cut -d '"' -f2)"
fi

case "$expected_arch" in
  arm64 | x86_64) ;;
  *)
    printf 'Unsupported expected architecture: %s\n' "$expected_arch" >&2
    usage >&2
    exit 1
    ;;
esac

versioned_dmg="$dist/CCCodexProxy-$version-macOS.dmg"
versioned_zip="$dist/CCCodexProxy-$version-macOS.zip"
latest_dmg="$dist/CCCodexProxy-macOS.dmg"
latest_zip="$dist/CCCodexProxy-macOS.zip"
checksums="$dist/SHA256SUMS"
manifest="$dist/RELEASE_MANIFEST.json"
app_binary="$dist/CCCodexProxy.app/Contents/MacOS/CCCodexProxy"
helper_binary="$dist/CCCodexProxy.app/Contents/Helpers/cc-codex-proxy"

required_files=(
  "$versioned_dmg"
  "$versioned_zip"
  "$latest_dmg"
  "$latest_zip"
  "$checksums"
  "$manifest"
  "$app_binary"
  "$helper_binary"
)

for path in "${required_files[@]}"; do
  if [[ ! -f "$path" ]]; then
    printf 'Missing release artifact: %s\n' "$path" >&2
    exit 1
  fi
done

hdiutil verify "$versioned_dmg"
hdiutil verify "$latest_dmg"

(
  cd "$dist"
  shasum -a 256 -c SHA256SUMS
)

verify_single_arch() {
  local binary="$1"
  local archs

  archs="$(lipo -archs "$binary" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
  if [[ "$archs" != "$expected_arch" ]]; then
    printf 'Expected %s to contain only %s, got: %s\n' "$binary" "$expected_arch" "$archs" >&2
    exit 1
  fi
}

verify_single_arch "$app_binary"
verify_single_arch "$helper_binary"

grep -q "\"architecture\": \"$expected_arch\"" "$manifest"
grep -q '"universal": false' "$manifest"

if [[ "$expected_arch" == "arm64" ]]; then
  grep -q 'depends_on arch: :arm64' "$repo_root/Casks/cc-codex-proxy-app.rb"
  grep -q 'depends_on arch: :arm64' "$repo_root/packaging/homebrew/cc-codex-proxy-app.rb"
fi

printf 'Verified release packaging for %s (%s)\n' "$version" "$expected_arch"
