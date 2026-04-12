#!/bin/bash
# Downloads pre-built lit binaries from the latest GitHub release
# into bin/ for plugin distribution.
set -euo pipefail

REPO="kl2792/lit"
BIN_DIR="$(cd "$(dirname "$0")/../bin" && pwd)"
mkdir -p "$BIN_DIR"

# Get latest release tag
TAG=$(curl -sL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | sed 's/.*: "//;s/".*//')
echo "Downloading $TAG binaries..."

for PLATFORM in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu; do
  for BIN in lit lit-mcp; do
    URL="https://github.com/$REPO/releases/download/$TAG/$BIN-$PLATFORM"
    echo "  $BIN-$PLATFORM"
    curl -sL "$URL" -o "$BIN_DIR/$BIN-$PLATFORM"
    chmod +x "$BIN_DIR/$BIN-$PLATFORM"
  done
done

echo "Done. Binaries in $BIN_DIR/"
