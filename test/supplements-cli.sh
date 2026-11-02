#!/bin/sh
# Regression: the original CLI rejects --supplements before doing any I/O.
set -eu
"$1" download --help | grep -- --supplements
"$1" download --help | grep -- --supplements-page
"$1" download --help | grep -- --from-discovery
scratch=$(mktemp -d "$PWD/tmp/supplements-cli.XXXXXX")
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
status=0
LIT_DB_PATH="$scratch/lit.db" "$1" download 2006.11239 --supplements --json > "$scratch/stdout" 2> "$scratch/stderr" || status=$?
test "$status" -eq 1
test ! -s "$scratch/stdout"
grep 'supplements require a DOI' "$scratch/stderr"
status=0
LIT_DB_PATH="$scratch/lit.db" "$1" download 10.3389/fncom.2020.00006 --supplements-page https://pmc.ncbi.nlm.nih.gov/articles/PMC7025464/ --json > "$scratch/page-stdout" 2> "$scratch/page-stderr" || status=$?
test "$status" -eq 2
test ! -s "$scratch/page-stdout"
grep -- --supplements "$scratch/page-stderr"
# Replaying a pinned discovery must work offline and retain literal article links.
LIT_DB_PATH="$scratch/lit.db" "$1" download 10.3389/fncom.2020.00006 --supplements --from-discovery "$PWD/test/fixtures/pmc" --url-only --json > "$scratch/replayed.json"
grep 'Supplementary Figure 7' "$scratch/replayed.json"
grep '/articles/instance/7025464/bin/Image_7.jpg' "$scratch/replayed.json"
mkdir "$scratch/tampered"
cp test/fixtures/pmc/landing.html test/fixtures/pmc/response.json "$scratch/tampered/"
printf '\n' >> "$scratch/tampered/landing.html"
status=0
LIT_DB_PATH="$scratch/lit.db" "$1" download 10.3389/fncom.2020.00006 --supplements --from-discovery "$scratch/tampered" --url-only --json > "$scratch/tampered.json" 2> "$scratch/tampered.err" || status=$?
test "$status" -eq 1
test ! -s "$scratch/tampered.json"
grep 'capture.*mismatch' "$scratch/tampered.err"
status=0
LIT_DB_PATH="$scratch/lit.db" "$1" download 10.1/different --supplements --from-discovery "$PWD/test/fixtures/pmc" --url-only --json > "$scratch/wrong-doi.json" 2> "$scratch/wrong-doi.err" || status=$?
test "$status" -eq 1
test ! -s "$scratch/wrong-doi.json"
grep 'does not confirm requested DOI' "$scratch/wrong-doi.err"
