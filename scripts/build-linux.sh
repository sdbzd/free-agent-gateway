#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "This script must be run on Linux." >&2
  exit 1
fi

PROFILE="${PROFILE:-release}"
TARGET="${TARGET:-}"
ARCH="$(uname -m)"
PACKAGE_NAME="free-agent-gateway-linux-${ARCH}"
OUT_DIR="${OUT_DIR:-"$ROOT_DIR/dist/$PACKAGE_NAME"}"

build_args=(--locked --profile "$PROFILE")
if [[ -n "$TARGET" ]]; then
  build_args+=(--target "$TARGET")
fi

cargo build "${build_args[@]}"

target_dir="$ROOT_DIR/target"
if [[ -n "$TARGET" ]]; then
  target_dir="$target_dir/$TARGET"
fi

binary="$target_dir/$PROFILE/free-agent-gateway"
if [[ ! -x "$binary" ]]; then
  echo "Binary not found: $binary" >&2
  exit 1
fi

rm -rf "$OUT_DIR"
install -d "$OUT_DIR"
install -m 0755 "$binary" "$OUT_DIR/free-agent-gateway"
install -m 0644 "$ROOT_DIR/config.yaml.sample" "$OUT_DIR/config.yaml.sample"

if [[ -f "$ROOT_DIR/README.md" ]]; then
  install -m 0644 "$ROOT_DIR/README.md" "$OUT_DIR/README.md"
fi

if [[ -f "$ROOT_DIR/LICENSE" ]]; then
  install -m 0644 "$ROOT_DIR/LICENSE" "$OUT_DIR/LICENSE"
fi

tarball="$OUT_DIR.tar.gz"
rm -f "$tarball"
tar -C "$(dirname "$OUT_DIR")" -czf "$tarball" "$(basename "$OUT_DIR")"

echo "Built: $OUT_DIR/free-agent-gateway"
echo "Package: $tarball"
