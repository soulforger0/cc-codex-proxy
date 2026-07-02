#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  cat <<'EOF'
Usage:
  scripts/update-homebrew-packaging.sh <version> <source_sha256> <dmg_sha256>

Environment fallbacks:
  SOURCE_SHA256       SHA256 for the GitHub tag source archive
  SOURCE_ARCHIVE      Local archive file to hash when SOURCE_SHA256 is unset
  DMG_SHA256          SHA256 for CCCodexProxy-<version>-macOS.dmg
  SHA256SUMS_PATH     Local SHA256SUMS file to read when DMG_SHA256 is unset

Example:
  scripts/update-homebrew-packaging.sh 0.3.0 \
    e5c4b6be544b8ac8e5293e6a18155a24ef183be0c5b57a37d205f9c2fa5a02d0 \
    07a1de3df5372119f31a658c8bc0b3c95f97ec38f3f7bc032576db65fa88cb87
EOF
}

version="${1:-}"
source_sha="${2:-${SOURCE_SHA256:-}}"
dmg_sha="${3:-${DMG_SHA256:-}}"

if [[ "${version:-}" == "-h" || "${version:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ -z "$version" ]]; then
  version="$(grep -m1 '^version = ' "$repo_root/crates/cc-codex-proxy/Cargo.toml" | cut -d '"' -f2)"
fi

if [[ -z "$source_sha" && -n "${SOURCE_ARCHIVE:-}" ]]; then
  source_sha="$(shasum -a 256 "$SOURCE_ARCHIVE" | awk '{print $1}')"
fi

if [[ -z "$dmg_sha" && -n "${SHA256SUMS_PATH:-}" ]]; then
  dmg_sha="$(awk -v artifact="CCCodexProxy-$version-macOS.dmg" '$2 == artifact {print $1}' "$SHA256SUMS_PATH")"
fi

if [[ ! "$version" =~ ^[0-9]+[.][0-9]+[.][0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  printf 'Invalid version: %s\n' "$version" >&2
  usage >&2
  exit 1
fi

if [[ ! "$source_sha" =~ ^[0-9a-f]{64}$ ]]; then
  printf 'Invalid or missing source sha256: %s\n' "${source_sha:-<empty>}" >&2
  usage >&2
  exit 1
fi

if [[ ! "$dmg_sha" =~ ^[0-9a-f]{64}$ ]]; then
  printf 'Invalid or missing DMG sha256: %s\n' "${dmg_sha:-<empty>}" >&2
  usage >&2
  exit 1
fi

formula_files=(
  "$repo_root/Formula/cc-codex-proxy.rb"
  "$repo_root/packaging/homebrew/cc-codex-proxy.rb"
)
cask_files=(
  "$repo_root/Casks/cc-codex-proxy-app.rb"
  "$repo_root/packaging/homebrew/cc-codex-proxy-app.rb"
)

for file in "${formula_files[@]}"; do
  VERSION="$version" SOURCE_SHA="$source_sha" perl -0pi -e '
    s#url "https://github\.com/soulforger0/cc-codex-proxy/archive/refs/tags/v[^"]+\.tar\.gz"#url "https://github.com/soulforger0/cc-codex-proxy/archive/refs/tags/v$ENV{VERSION}.tar.gz"#;
    s#sha256 "([0-9a-f]{64}|PLACEHOLDER)"#sha256 "$ENV{SOURCE_SHA}"#;
  ' "$file"
done

for file in "${cask_files[@]}"; do
  VERSION="$version" DMG_SHA="$dmg_sha" perl -0pi -e '
    s#version "[^"]+"#version "$ENV{VERSION}"#;
    s#sha256 "([0-9a-f]{64}|PLACEHOLDER)"#sha256 "$ENV{DMG_SHA}"#;
  ' "$file"
done

ruby -c "$repo_root/Formula/cc-codex-proxy.rb" >/dev/null
ruby -c "$repo_root/Casks/cc-codex-proxy-app.rb" >/dev/null
ruby -c "$repo_root/packaging/homebrew/cc-codex-proxy.rb" >/dev/null
ruby -c "$repo_root/packaging/homebrew/cc-codex-proxy-app.rb" >/dev/null

printf 'Updated Homebrew metadata for %s\n' "$version"
