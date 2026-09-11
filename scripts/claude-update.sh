#!/usr/bin/env bash
#
# claude-update.sh — update Claude Code from the reachable GCS release bucket.
#
# Why this exists: Claude Code's built-in `claude update` fetches from
# https://downloads.claude.ai, which is IP/edge-blocked in some regions. DNS
# resolves but TCP connects time out, so a hosts-file override cannot help.
# The same release artifacts are mirrored on Google Cloud Storage, which is
# reachable, and that is what this script pulls from.
#
# It performs the SAME two-step handoff the proxy already implements for
# `claude update` (see proxy-core: begin_shim_update/finish_shim_update):
#   1. release the managed shim back to a symlink pointing at the new version
#   2. run `claude install-shim` so CC Codex Proxy recaptures the target,
#      rewrites claude-shim.json, and regenerates the launcher
#
# Safe by design: the current version is untouched until the download is
# checksum-verified, and any failure after the shim is released restores it.
#
# Usage: scripts/claude-update.sh [version]
#   (no args)  -> latest stable from the GCS `stable` channel
#   <version>  -> install that exact version, e.g. 2.1.236
#
set -euo pipefail

readonly BUCKET="https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases"
readonly VERSIONS_DIR="${HOME}/.local/share/claude/versions"
readonly SHIM_PATH="${HOME}/.local/bin/claude"
readonly STATE_FILE="${HOME}/Library/Application Support/CCCodexProxy/claude-shim.json"

HELPER=""
SHIM_BACKUP=""
STATE_BACKUP=""
TMP=""
SHIM_RELEASED=0

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
info() { printf '==> %s\n' "$*"; }

restore_snapshots() {
  # SHIM_PATH may currently be a symlink to the newly installed Claude binary.
  # Remove it first so cp cannot follow the link and overwrite that binary.
  rm -f "$SHIM_PATH"
  cp -f "$SHIM_BACKUP" "$SHIM_PATH"
  chmod +x "$SHIM_PATH"
  cp -f "$STATE_BACKUP" "$STATE_FILE"
  SHIM_RELEASED=0
}

# Clean up temp files, and restore the shim if we die after releasing it
# (before install-shim re-created it).
cleanup() {
  local code=$?
  [[ -n $TMP && -f $TMP ]] && rm -f "$TMP"
  if [[ $SHIM_RELEASED -eq 1 && $code -ne 0 && -n $SHIM_BACKUP && -f $SHIM_BACKUP ]]; then
    printf '==> update failed; restoring previous managed shim\n' >&2
    restore_snapshots
  fi
  [[ -n $SHIM_BACKUP && -f $SHIM_BACKUP ]] && rm -f "$SHIM_BACKUP"
  [[ -n $STATE_BACKUP && -f $STATE_BACKUP ]] && rm -f "$STATE_BACKUP"
  return 0
}
trap cleanup EXIT

need() { command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"; }
need curl
need python3
need shasum

[[ "$(uname -s)" == "Darwin" ]] || die "this script targets macOS; adapt the platform mapping for other OSes"
case "$(uname -m)" in
  arm64) PLATFORM="darwin-arm64" ;;
  x86_64) PLATFORM="darwin-x64" ;;
  *) die "unsupported architecture: $(uname -m)" ;;
esac

# --- Resolve the helper binary from the current shim -------------------------
[[ -f $SHIM_PATH ]] || die "no Claude shim at $SHIM_PATH"
[[ ! -L $SHIM_PATH ]] || die "$SHIM_PATH is a symlink, not a managed shim; repair the shim before updating"
if ! grep -q "CC_CODEX_PROXY_MANAGED_CLAUDE_SHIM" "$SHIM_PATH"; then
  die "$SHIM_PATH is not a CC Codex Proxy managed shim (run this with the proxy shim active)"
fi
[[ -f $STATE_FILE ]] || die "managed shim state not found: $STATE_FILE"
HELPER="$(sed -n "s/^exec '\([^']*\)'.*/\1/p" "$SHIM_PATH" | head -1)"
[[ -x $HELPER ]] || die "helper binary from shim is not executable: $HELPER"

# Reuse the exact settings already encoded in the shim so we don't drift.
# Values may be single-quoted (paths) or bare (scalars); handle both.
shim_arg() {
  sed -n "s/^ *--$1  *'\([^']*\)'.*/\1/p; s/^ *--$1  *\([^' ][^ ]*\) *.*/\1/p" \
    "$SHIM_PATH" | head -1
}
PROVIDER="$(shim_arg provider)";  APP_PID="$(shim_arg app-pid)"
PORT="$(shim_arg port)";          MODEL="$(shim_arg model)"
SMALL_MODEL="$(shim_arg small-model)"; WINDOW="$(shim_arg auto-compact-window)"
[[ -n $PROVIDER && -n $MODEL && -n $SMALL_MODEL ]] || die "managed shim is missing required settings"
[[ $APP_PID =~ ^[0-9]+$ && $PORT =~ ^[0-9]+$ && $WINDOW =~ ^[0-9]+$ ]] || \
  die "managed shim contains invalid numeric settings"

# --- Resolve target version + expected checksum ------------------------------
TARGET="${1:-$(curl -fsSL --retry 3 -m 30 "$BUCKET/stable")}"
[[ $TARGET =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "unexpected version string: '$TARGET'"
info "target version: $TARGET"

MANIFEST="$(curl -fsSL --retry 3 -m 30 "$BUCKET/$TARGET/manifest.json")"
read -r EXPECTED_SHA EXPECTED_SIZE <<<"$(printf '%s' "$MANIFEST" | python3 -c '
import json,sys
p=json.load(sys.stdin)["platforms"][sys.argv[1]]
print(p["checksum"], p["size"])
' "$PLATFORM")"
[[ $EXPECTED_SHA =~ ^[[:xdigit:]]{64}$ && $EXPECTED_SIZE =~ ^[0-9]+$ ]] || \
  die "could not read valid artifact metadata for $PLATFORM from manifest"

DEST="$VERSIONS_DIR/$TARGET"
ACTUAL_SHA=""
if [[ -f $DEST ]]; then
  info "$DEST already exists; verifying checksum"
  ACTUAL_SHA="$(shasum -a 256 "$DEST" | awk '{print $1}')"
fi

if [[ $ACTUAL_SHA != "$EXPECTED_SHA" ]]; then
  if [[ -n $ACTUAL_SHA ]]; then
    info "existing artifact failed checksum; replacing it"
  fi
  # --- Download to a temp file, verify, then install atomically --------------
  TMP="$(mktemp "$VERSIONS_DIR/.$TARGET.XXXXXX")"
  info "downloading $PLATFORM ($((EXPECTED_SIZE/1024/1024)) MB)..."
  curl -fL --retry 3 -m 1800 "$BUCKET/$TARGET/$PLATFORM/claude" -o "$TMP"
  ACTUAL_SHA="$(shasum -a 256 "$TMP" | awk '{print $1}')"
  [[ $ACTUAL_SHA == "$EXPECTED_SHA" ]] || die "checksum mismatch (expected $EXPECTED_SHA, got $ACTUAL_SHA)"
  info "checksum verified"
  chmod +x "$TMP"
  mv -f "$TMP" "$DEST"
  TMP=""
fi

# A valid checksum is the primary integrity check. Keep the execution probe
# bounded because Claude startup can otherwise leave the updater stuck forever.
if ! python3 - "$DEST" <<'PY'
import subprocess
import sys

try:
    result = subprocess.run(
        [sys.argv[1], "--version"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=30,
    )
except subprocess.TimeoutExpired:
    raise SystemExit(124)
raise SystemExit(result.returncode)
PY
then
  die "downloaded binary failed to run within 30 seconds: $DEST"
fi

# --- Hand off to the proxy's own shim machinery ------------------------------
# Snapshot state + shim so a failure mid-handoff is recoverable.
SHIM_BACKUP="$(mktemp)"
cp -f "$SHIM_PATH" "$SHIM_BACKUP"
STATE_BACKUP="$(mktemp)"
cp -f "$STATE_FILE" "$STATE_BACKUP"

info "releasing managed shim (native launcher -> $TARGET)"
rm -f "$SHIM_PATH"
ln -s "$DEST" "$SHIM_PATH"
SHIM_RELEASED=1

info "reinstalling managed shim against $TARGET"
if ! "$HELPER" claude install-shim \
      --provider "$PROVIDER" --app-pid "$APP_PID" \
      --claude-path "$SHIM_PATH" --port "$PORT" \
      --model "$MODEL" --small-model "$SMALL_MODEL" \
      --auto-compact-window "$WINDOW"; then
  info "install-shim failed; restoring previous shim and state" >&2
  restore_snapshots
  die "shim reinstall failed; state restored"
fi
SHIM_RELEASED=0

info "done"
printf '%-24s %s\n' "version:" "$("$SHIM_PATH" --version 2>/dev/null)"
printf '%-24s %s\n' "shim:" "$SHIM_PATH"
printf '%-24s %s\n' "real claude:" "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["shims"][0]["realClaudePath"])' "$STATE_FILE")"
