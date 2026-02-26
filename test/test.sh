#!/bin/bash
# Test cases for sh/lit.sh
# Tests: auto-detect, explicit commands, edge cases, LLM-friendliness
#
# Usage: sh/lit_test.sh [parallel_jobs]

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LIT="$SCRIPT_DIR/../bin/lit"
PARALLEL_JOBS="${1:-8}"

export LIT_CACHE_DIR="${LIT_CACHE_DIR:-$SCRIPT_DIR/cache}"
mkdir -p "$LIT_CACHE_DIR"

# Colors (only in terminal)
if [ -t 1 ]; then
    GREEN='\033[0;32m'; RED='\033[0;31m'; NC='\033[0m'
else
    GREEN=''; RED=''; NC=''
fi

# Test definitions: "name|command|args|expected_pattern"
TESTS=(
    # ── Auto-detect (the killer feature) ──
    "auto:arxiv-id|auto|2006.11239|Denoising Diffusion"
    "auto:arxiv-url|auto|https://arxiv.org/abs/2006.11239|Denoising Diffusion"
    "auto:arxiv-pdf-url|auto|https://arxiv.org/pdf/2006.11239v2|Denoising Diffusion"
    "auto:doi|auto|10.1145/3442188.3445899|Algorithmic Recourse"
    "auto:doi-url|auto|https://doi.org/10.1145/3442188.3445899|Algorithmic Recourse"
    "auto:isbn|auto|978-0262039246|Reinforcement Learning"
    "auto:free-text|auto|attention is all you need|Vaswani"

    # ── Explicit commands ──
    "search:full-title|search|A Unified Approach to Interpreting Model Predictions|Lundberg"
    "search:Attention|search|Attention is all you need Vaswani|Attention"
    "dblp:Sundararajan|dblp|Axiomatic attribution deep networks Sundararajan|Sundararajan"
    "doi:Karimi2021|doi|10.1145/3442188.3445899|Algorithmic Recourse"
    "bibtex:Karimi2021|bibtex|10.1145/3442188.3445899|@"
    "arxiv:RUDDER|arxiv|1806.07857|RUDDER"
    "arxiv:Diffusion|arxiv|2006.11239|Denoising"
    "isbn:Sutton|isbn|978-0262039246|Reinforcement Learning"
    "book:Pearl|book|Causality Pearl|Pearl"

    # ── URL normalization ──
    "norm:arxiv-prefix|arxiv|arXiv:2006.11239|Denoising"
    "norm:doi-url|doi|https://doi.org/10.1145/3442188.3445899|Algorithmic"

    # ── PDF / refs / cites ──
    "pdf:Karimi|pdf|10.1145/3442188.3445899|pdf"
    "refs:SHAP|refs|arXiv:1705.07874|."
    "cites:SHAP|cites|arXiv:1705.07874|."

    # ── Edge cases: no crash ──
    "edge:no-args-search|search||Usage"
    "edge:no-args-arxiv|arxiv||Usage"
    "edge:no-args-doi|doi||Usage"
    "edge:help|help||lit.sh"
)

run_single_test() {
    local test_spec="$1"
    local name cmd args expected
    IFS='|' read -r name cmd args expected <<< "$test_spec"

    local result
    if [ "$cmd" = "auto" ]; then
        # Auto-detect: pass args directly as first arg (no command)
        result=$("$LIT" "$args" 2>&1 || true)
    elif [ -z "$args" ]; then
        result=$("$LIT" "$cmd" 2>&1 || true)
    else
        result=$("$LIT" "$cmd" "$args" 3 2>&1 || true)
    fi

    if echo "$result" | grep -qi "$expected"; then
        echo "PASS|$name"
    else
        # Include first line of result for debugging
        local first=$(echo "$result" | head -1 | cut -c1-60)
        echo "FAIL|$name|expected '$expected' got: $first"
    fi
}

export -f run_single_test
export LIT LIT_CACHE_DIR

echo "========================================"
echo "lit.sh tests (parallel=$PARALLEL_JOBS)"
echo "========================================"
echo ""

# Run tests in parallel
results=$(printf '%s\n' "${TESTS[@]}" | xargs -P "$PARALLEL_JOBS" -I {} bash -c 'run_single_test "$@"' _ {})

# Count results
passed=$(echo "$results" | grep -c "^PASS" || true)
failed=$(echo "$results" | grep -c "^FAIL" || true)
total=${#TESTS[@]}

# Show results grouped
echo "── Auto-detect ──"
echo "$results" | grep "auto:" | while IFS='|' read -r status name detail; do
    if [ "$status" = "PASS" ]; then
        echo -e "  ${GREEN}ok${NC} $name"
    else
        echo -e "  ${RED}FAIL${NC} $name ($detail)"
    fi
done

echo "── Commands ──"
echo "$results" | grep -E "^(PASS|FAIL)\|(search|dblp|doi|bibtex|arxiv|isbn|book|pdf|refs|cites):" | while IFS='|' read -r status name detail; do
    if [ "$status" = "PASS" ]; then
        echo -e "  ${GREEN}ok${NC} $name"
    else
        echo -e "  ${RED}FAIL${NC} $name ($detail)"
    fi
done

echo "── Normalization ──"
echo "$results" | grep "norm:" | while IFS='|' read -r status name detail; do
    if [ "$status" = "PASS" ]; then
        echo -e "  ${GREEN}ok${NC} $name"
    else
        echo -e "  ${RED}FAIL${NC} $name ($detail)"
    fi
done

echo "── Edge cases ──"
echo "$results" | grep "edge:" | while IFS='|' read -r status name detail; do
    if [ "$status" = "PASS" ]; then
        echo -e "  ${GREEN}ok${NC} $name"
    else
        echo -e "  ${RED}FAIL${NC} $name ($detail)"
    fi
done

echo ""
echo "========================================"
echo "Results: $passed/$total passed, $failed failed"
echo "========================================"

[ "$failed" -eq 0 ]
