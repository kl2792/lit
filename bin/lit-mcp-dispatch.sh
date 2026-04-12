#!/bin/sh
DIR="$(cd "$(dirname "$0")" && pwd)"
ARCH="$(uname -m)"
OS="$(uname -s)"
case "$OS-$ARCH" in
  Darwin-arm64)  exec "$DIR/lit-mcp-aarch64-apple-darwin" "$@" ;;
  Darwin-x86_64) exec "$DIR/lit-mcp-x86_64-apple-darwin" "$@" ;;
  Linux-x86_64)  exec "$DIR/lit-mcp-x86_64-unknown-linux-gnu" "$@" ;;
  *) echo "Unsupported platform: $OS-$ARCH" >&2; exit 1 ;;
esac
