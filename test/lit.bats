#!/usr/bin/env bats
# lit — bats test suite
# Uses cached API responses in test/cache/ for offline reproducibility.
# Run: bats test/lit.bats  OR  make test

setup() {
    export LIT="$BATS_TEST_DIRNAME/../bin/lit"
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

# ── Explicit commands ────────────────────────────────────────────────

@test "search: full title" {
    run "$LIT" search "A Unified Approach to Interpreting Model Predictions"
    [[ "$output" =~ "Lundberg" ]]
}

@test "search: with author" {
    run "$LIT" search "Attention is all you need Vaswani" 3
    [[ "$output" =~ "Attention" ]]
}

@test "doi: lookup" {
    run "$LIT" doi 10.1145/3442188.3445899
    [[ "$output" =~ "Algorithmic Recourse" ]]
}

@test "bibtex: generates entry" {
    run "$LIT" bibtex 10.1145/3442188.3445899
    [[ "$output" =~ "@" ]]
}

@test "arxiv: RUDDER" {
    run "$LIT" arxiv 1806.07857
    [[ "$output" =~ "RUDDER" ]]
}

@test "arxiv: Diffusion" {
    run "$LIT" arxiv 2006.11239
    [[ "$output" =~ "Denoising" ]]
}

@test "isbn: Sutton RL" {
    run "$LIT" isbn 978-0262039246
    [[ "$output" =~ "Reinforcement Learning" ]]
}

@test "book: Pearl Causality" {
    run "$LIT" book "Causality Pearl" 3
    [[ "$output" =~ "Pearl" ]]
}

# ── URL normalization ────────────────────────────────────────────────

@test "normalize: arXiv: prefix" {
    run "$LIT" arxiv arXiv:2006.11239
    [[ "$output" =~ "Denoising" ]]
}

@test "normalize: doi URL" {
    run "$LIT" doi https://doi.org/10.1145/3442188.3445899
    [[ "$output" =~ "Algorithmic" ]]
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

# ── Edge cases ───────────────────────────────────────────────────────

@test "no args: search shows usage" {
    run "$LIT" search
    [[ "$output" =~ "Usage" ]] || [[ "$output" =~ "usage" ]] || [[ "$output" =~ "Error" ]]
}

@test "no args: arxiv shows usage" {
    run "$LIT" arxiv
    [[ "$output" =~ "Usage" ]] || [[ "$output" =~ "usage" ]] || [[ "$output" =~ "Error" ]]
}

@test "no args: doi shows usage" {
    run "$LIT" doi
    [[ "$output" =~ "Usage" ]] || [[ "$output" =~ "usage" ]] || [[ "$output" =~ "Error" ]]
}

@test "help: shows usage" {
    run "$LIT" help
    [[ "$output" =~ "lit" ]]
}

@test "help flag: -h" {
    run "$LIT" -h
    [[ "$output" =~ "lit" ]]
}

@test "verbose flag: -v" {
    run "$LIT" -v arxiv 2006.11239
    [[ "$output" =~ "Denoising" ]]
}

# ── Output format (LLM-friendly) ────────────────────────────────────

@test "no ANSI codes when piped" {
    result=$("$LIT" arxiv 2006.11239 2>&1)
    if echo "$result" | grep -qP '\033\['; then
        echo "Found ANSI escape codes in output"
        false
    fi
}
