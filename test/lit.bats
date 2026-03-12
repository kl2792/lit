#!/usr/bin/env bats
# lit -- bats test suite
# Uses cached API responses in test/cache/ for offline reproducibility.
# Run: bats test/lit.bats  OR  make test

setup() {
    # Use pre-set LIT or default to Rust binary
    if [ -z "${LIT:-}" ]; then
        if [ -x "$BATS_TEST_DIRNAME/../target/release/lit" ]; then
            export LIT="$BATS_TEST_DIRNAME/../target/release/lit"
        else
            export LIT="$BATS_TEST_DIRNAME/../target/debug/lit"
        fi
    fi
    export LIT_CACHE_DIR="$BATS_TEST_DIRNAME/cache"
    mkdir -p "$LIT_CACHE_DIR"
}

# ── Auto-detect ──────────────────────────────────────────────────────

@test "auto-detect: arXiv ID" {
    run "$LIT" 2006.11239
    [[ "$output" =~ "Denoising Diffusion" ]]
}

@test "auto-detect: arXiv URL" {
    run "$LIT" https://arxiv.org/abs/2006.11239
    [[ "$output" =~ "Denoising Diffusion" ]]
}

@test "auto-detect: arXiv PDF URL" {
    run "$LIT" https://arxiv.org/pdf/2006.11239v2
    [[ "$output" =~ "Denoising Diffusion" ]]
}

@test "auto-detect: DOI" {
    run "$LIT" 10.1145/3442188.3445899
    [[ "$output" =~ "Algorithmic Recourse" ]]
}

@test "auto-detect: DOI URL" {
    run "$LIT" https://doi.org/10.1145/3442188.3445899
    [[ "$output" =~ "Algorithmic Recourse" ]]
}

@test "auto-detect: ISBN" {
    run "$LIT" 978-0262039246
    [[ "$output" =~ "Reinforcement Learning" ]]
}

@test "auto-detect: free text searches" {
    run "$LIT" "attention is all you need"
    [[ "$output" =~ "Vaswani" ]] || [[ "$output" =~ "Attention" ]]
}

@test "auto-detect: arXiv prefix" {
    run "$LIT" arXiv:2006.11239
    [[ "$output" =~ "Denoising" ]]
}

@test "auto-detect: DOI URL normalization" {
    run "$LIT" https://doi.org/10.1145/3442188.3445899
    [[ "$output" =~ "Algorithmic" ]]
}

# ── Search ───────────────────────────────────────────────────────────

@test "search: full title" {
    run "$LIT" search "A Unified Approach to Interpreting Model Predictions"
    [[ "$output" =~ "Lundberg" ]]
}

@test "search: with limit" {
    run "$LIT" search -l 3 "attention is all you need"
    [[ "$output" =~ "Attention" ]] || [[ "$output" =~ "attention" ]]
}

@test "search: DBLP source" {
    run "$LIT" search -s dblp "attention is all you need"
    [ "$status" -eq 0 ]
}

@test "search: book source" {
    run "$LIT" search -s book "Causality Pearl"
    [[ "$output" =~ "Pearl" ]] || [[ "$output" =~ "Causality" ]]
}

@test "search: empty query" {
    run "$LIT" search
    [[ "$output" =~ "Usage" ]] || [[ "$output" =~ "usage" ]] || [[ "$output" =~ "error" ]]
}

# ── PDF / refs / cites ──────────────────────────────────────────────

@test "pdf: finds URL" {
    run "$LIT" pdf 10.1145/3442188.3445899
    [[ "$output" =~ "pdf" ]] || [[ "$output" =~ "PDF" ]]
}

@test "refs: gets references" {
    run "$LIT" refs arXiv:1705.07874
    [ "$status" -eq 0 ]
}

@test "cites: gets citations" {
    run "$LIT" cites arXiv:1705.07874
    [ "$status" -eq 0 ]
}

@test "refs: bare DOI auto-prefixed" {
    run "$LIT" refs 10.1093/bjps/axi147
    [ "$status" -eq 0 ]
}

@test "cites: bare arXiv ID auto-prefixed with ARXIV:" {
    run "$LIT" cites 2004.12265
    [ "$status" -eq 0 ]
    [[ "$output" =~ "Causal" ]] || [[ "$output" =~ "Interpret" ]] || [[ "$output" != "" ]]
}

# ── Flags ────────────────────────────────────────────────────────────

@test "help flag: -h" {
    run "$LIT" -h
    [[ "$output" =~ "lit" ]]
}

@test "verbose flag: -v" {
    run "$LIT" -v 2006.11239
    [[ "$output" =~ "Denoising" ]]
}

@test "bib flag: -b prints bibtex to stdout" {
    run "$LIT" -b 2006.11239
    [[ "$output" =~ "@article" ]]
}

@test "json flag: --json" {
    run "$LIT" --json 2006.11239
    [[ "$output" =~ '"title"' ]]
    [[ "$output" =~ '"year"' ]]
}

@test "json flag: --json search" {
    run "$LIT" --json search "attention is all you need"
    [[ "$output" =~ '"title"' ]]
}

# ── Search quality (E2E, cached responses) ─────────────────────────

@test "search quality: Halpern actual causation" {
    run "$LIT" search "halpern actual causes" -l 5
    [[ "$output" =~ "Halpern" ]]
    [[ "$output" =~ "Causes" ]] || [[ "$output" =~ "causes" ]] || [[ "$output" =~ "Causality" ]]
}

@test "search quality: Pearl Causality book" {
    run "$LIT" search "pearl causality 2009 book" -l 5
    [[ "$output" =~ "Pearl" ]]
    [[ "$output" =~ "Causality" ]] || [[ "$output" =~ "causal" ]]
}

@test "search quality: Schulman PPO" {
    run "$LIT" search "PPO proximal policy optimization schulman" -l 5
    [[ "$output" =~ "Schulman" ]] || [[ "$output" =~ "Proximal Policy Optimization" ]]
}

@test "search quality: double descent" {
    run "$LIT" search "double descent bias variance" -l 5
    [[ "$output" =~ "Double" ]] || [[ "$output" =~ "double" ]]
    [[ "$output" =~ "Descent" ]] || [[ "$output" =~ "descent" ]]
}

# ── Color control ───────────────────────────────────────────────────

@test "no ANSI codes when piped" {
    result=$("$LIT" 2006.11239 2>&1)
    if echo "$result" | grep -qP '\033\['; then
        echo "Found ANSI escape codes in output"
        false
    fi
}

@test "no-color flag: --no-color" {
    result=$("$LIT" --no-color -v 2006.11239 2>&1)
    if echo "$result" | grep -qP '\033\['; then
        echo "Found ANSI escape codes in --no-color output"
        false
    fi
}

@test "NO_COLOR env var" {
    result=$(NO_COLOR=1 "$LIT" -v 2006.11239 2>&1)
    if echo "$result" | grep -qP '\033\['; then
        echo "Found ANSI escape codes with NO_COLOR=1"
        false
    fi
}
