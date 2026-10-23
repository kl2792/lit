#!/bin/sh
# Regression: the original CLI rejects --supplements before doing any I/O.
set -eu
"$1" download --help | grep -- --supplements
scratch=$(mktemp -d "$PWD/tmp/supplements-cli.XXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
status=0
LIT_DB_PATH="$scratch/lit.db" "$1" download 2006.11239 --supplements --json > "$scratch/stdout" 2> "$scratch/stderr" || status=$?
test "$status" -eq 1
test ! -s "$scratch/stdout"
grep 'supplements require a DOI' "$scratch/stderr"
