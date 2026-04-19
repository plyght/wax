#!/usr/bin/env bash
# wax vs Homebrew benchmark script
# Run from the repo root: bash benchmark.sh
# Requires: wax (release build or on PATH), brew

set -euo pipefail

WAX="${WAX:-$(command -v wax 2>/dev/null || echo ./target/release/wax)}"
BREW="${BREW:-$(command -v brew 2>/dev/null || echo brew)}"
RUNS="${RUNS:-3}"

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

die() { echo -e "${RED}error: $*${NC}" >&2; exit 1; }

[ -x "$WAX" ]  || die "wax not found at $WAX — set WAX=/path/to/wax or build with 'cargo build --release'"
command -v "$BREW" &>/dev/null || die "brew not found — install Homebrew/Linuxbrew first"
command -v bc &>/dev/null || die "bc required for float math"

# ---------- helpers -----------------------------------------------------------

timeit() {
    local t
    t=$( { time "$@" >/dev/null 2>&1; } 2>&1 | grep real | sed 's/.*m//;s/s//')
    # Ensure we have a valid number
    if [[ -z "$t" || ! "$t" =~ ^[0-9.]+$ ]]; then
        echo "0.000"
    else
        echo "$t"
    fi
}

avg() {
    local count=$#
    if [[ $count -eq 0 ]]; then
        echo "0.000"
        return
    fi
    local sum=0
    for v in "$@"; do
        # Skip empty or invalid values
        if [[ -n "$v" && "$v" =~ ^[0-9.]+$ ]]; then
            sum=$(echo "scale=3; $sum + $v" | bc)
        fi
    done
    echo "scale=3; $sum / $count" | bc
}

speedup() {
    # Handle empty or zero values
    if [[ -z "$1" || -z "$2" || "$2" == "0" || "$1" == "0" ]]; then
        echo "N/A"
        return
    fi
    echo "scale=1; $1 / $2" | bc
}

bench() {
    local label="$1"; shift
    local times=()
    for i in $(seq 1 "$RUNS"); do
        local t; t=$(timeit "$@")
        times+=("$t")
        printf "    run %-2s %ss\n" "$i" "$t"
    done
    local a; a=$(avg "${times[@]}")
    printf "    ${BOLD}avg  %ss${NC}   (%s)\n" "$a" "$label"
    echo "$a"
}

# ---------- system info -------------------------------------------------------

echo -e "\n${BOLD}=== System ===${NC}"
if command -v fastfetch &>/dev/null; then
    fastfetch --logo none 2>/dev/null | grep -E "OS:|Kernel:|CPU:|Memory:|Host:" | sed 's/^\s*/  /'
else
    echo "  OS:     $(uname -sr)"
    echo "  CPU:    $(grep 'model name' /proc/cpuinfo 2>/dev/null | head -1 | cut -d: -f2 | xargs || sysctl -n machdep.cpu.brand_string 2>/dev/null)"
    echo "  RAM:    $(free -h 2>/dev/null | awk '/^Mem/{print $2}' || sysctl -n hw.memsize 2>/dev/null | awk '{printf "%.0f GiB\n", $1/1073741824}')"
fi
echo "  wax:    $("$WAX" --version 2>/dev/null | head -1)"
echo "  brew:   $("$BREW" --version | head -1)"
echo "  runs:   $RUNS per benchmark"

# ---------- 1. update ---------------------------------------------------------

echo -e "\n${BOLD}=== 1. Update (index/formula sync) ===${NC}"

echo -e "\n  ${CYAN}wax update (warm cache)${NC}"
"$WAX" update >/dev/null 2>&1   # prime
wax_update=$(bench "wax warm" "$WAX" update)

echo -e "\n  ${CYAN}brew update (warm cache)${NC}"
"$BREW" update >/dev/null 2>&1  # prime
brew_update=$(bench "brew warm" "$BREW" update)

echo -e "\n  speedup: ${GREEN}$(speedup "$brew_update" "$wax_update") faster${NC}"

# ---------- 2. search ---------------------------------------------------------

echo -e "\n${BOLD}=== 2. Search (nginx) ===${NC}"

echo -e "\n  ${CYAN}wax search nginx${NC}"
wax_search=$(bench "wax" "$WAX" search nginx)

echo -e "\n  ${CYAN}brew search nginx${NC}"
brew_search=$(bench "brew" "$BREW" search nginx)

echo -e "\n  speedup: ${GREEN}$(speedup "$brew_search" "$wax_search") faster${NC}"

# ---------- 3. info -----------------------------------------------------------

echo -e "\n${BOLD}=== 3. Info (nginx) ===${NC}"

echo -e "\n  ${CYAN}wax info nginx${NC}"
wax_info=$(bench "wax" "$WAX" info nginx)

echo -e "\n  ${CYAN}brew info nginx${NC}"
brew_info=$(bench "brew" "$BREW" info nginx)

echo -e "\n  speedup: ${GREEN}$(speedup "$brew_info" "$wax_info") faster${NC}"

# ---------- 4. single-package install ----------------------------------------

echo -e "\n${BOLD}=== 4. Install: tree (single package, cold) ===${NC}"
echo "  (uninstalling tree from wax before each run)"

wax_tree_times=()
for i in $(seq 1 "$RUNS"); do
    "$WAX" uninstall tree >/dev/null 2>&1 || true
    t=$(timeit "$WAX" install tree --user)
    wax_tree_times+=("$t")
    printf "    run %-2s %ss\n" "$i" "$t"
done
wax_tree=$(avg "$(IFS=,; echo "${wax_tree_times[*]}")")
printf "    ${BOLD}avg  %ss${NC}   (wax --user)\n" "$wax_tree"

echo -e "\n  ${CYAN}brew install tree (cold)${NC}"
brew_tree_times=()
for i in $(seq 1 "$RUNS"); do
    "$BREW" uninstall --force tree >/dev/null 2>&1 || true
    t=$(timeit "$BREW" install tree)
    brew_tree_times+=("$t")
    printf "    run %-2s %ss\n" "$i" "$t"
done
brew_tree=$(avg "$(IFS=,; echo "${brew_tree_times[*]}")")
printf "    ${BOLD}avg  %ss${NC}   (brew)\n" "$brew_tree"

echo -e "\n  speedup: ${GREEN}$(speedup "$brew_tree" "$wax_tree") faster${NC}"

# ---------- 5. multi-package parallel install ---------------------------------

echo -e "\n${BOLD}=== 5. Install: ripgrep + bat + fd (multi-package, cold) ===${NC}"

wax_multi_times=()
for i in $(seq 1 "$RUNS"); do
    "$WAX" uninstall ripgrep bat fd >/dev/null 2>&1 || true
    t=$(timeit "$WAX" install ripgrep bat fd --user)
    wax_multi_times+=("$t")
    printf "    run %-2s %ss\n" "$i" "$t"
done
wax_multi=$(avg "$(IFS=,; echo "${wax_multi_times[*]}")")
printf "    ${BOLD}avg  %ss${NC}   (wax --user, parallel)\n" "$wax_multi"

echo -e "\n  ${CYAN}brew install ripgrep bat fd (cold)${NC}"
brew_multi_times=()
for i in $(seq 1 "$RUNS"); do
    "$BREW" uninstall --force ripgrep bat fd >/dev/null 2>&1 || true
    t=$(timeit "$BREW" install ripgrep bat fd)
    brew_multi_times+=("$t")
    printf "    run %-2s %ss\n" "$i" "$t"
done
brew_multi=$(avg "$(IFS=,; echo "${brew_multi_times[*]}")")
printf "    ${BOLD}avg  %ss${NC}   (brew, sequential)\n" "$brew_multi"

echo -e "\n  speedup: ${GREEN}$(speedup "$brew_multi" "$wax_multi") faster${NC}"

# ---------- summary -----------------------------------------------------------

echo -e "\n${BOLD}=== Summary ===${NC}"
printf "\n  %-35s %10s %10s %10s\n" "Benchmark" "wax" "brew" "speedup"
printf  "  %-35s %10s %10s %10s\n" "---------" "---" "----" "-------"
printf  "  %-35s %9ss %9ss %10s\n" "update (warm)"     "$wax_update" "$brew_update" "$(speedup "$brew_update" "$wax_update")"
printf  "  %-35s %9ss %9ss %10s\n" "search nginx"      "$wax_search" "$brew_search" "$(speedup "$brew_search" "$wax_search")"
printf  "  %-35s %9ss %9ss %10s\n" "info nginx"        "$wax_info"   "$brew_info"   "$(speedup "$brew_info"   "$wax_info")"
printf  "  %-35s %9ss %9ss %10s\n" "install tree"      "$wax_tree"   "$brew_tree"   "$(speedup "$brew_tree"   "$wax_tree")"
printf  "  %-35s %9ss %9ss %10s\n" "install ripgrep+bat+fd" "$wax_multi" "$brew_multi" "$(speedup "$brew_multi" "$wax_multi")"
echo ""
