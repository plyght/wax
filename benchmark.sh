#!/usr/bin/env bash
# wax vs Homebrew benchmark script.
# Run from the repo root:
#   bash benchmark.sh
#   bash benchmark.sh --with-installs --runs 3
#
# Install benchmarks uninstall/reinstall packages. They are skipped by default.

set -euo pipefail

WAX="$(command -v wax 2>/dev/null || echo ./target/release/wax)"
BREW="$(command -v brew 2>/dev/null || echo brew)"
RUNS=3
WITH_INSTALLS=0
DOWNLOAD_PROBE=1
CURL_CONNECT_TIMEOUT=10
CURL_MAX_TIME=120

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

usage() {
    cat <<'EOF'
Usage: bash benchmark.sh [OPTIONS]

Options:
  --with-installs       Run destructive install benchmarks.
  --runs N              Number of runs per benchmark (default: 3).
  --wax PATH            wax binary to benchmark (default: PATH wax or ./target/release/wax).
  --brew PATH           brew binary to benchmark (default: PATH brew).
  --no-download-probe   Skip bottle CDN download-only probes.
  --curl-timeout SECS   Max seconds for each download probe (default: 120).
  -h, --help            Show this help.

Install benchmarks uninstall/reinstall tree, ripgrep, bat, and fd.
EOF
}

die() { echo -e "${RED}error: $*${NC}" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --with-installs|--installs)
            WITH_INSTALLS=1
            shift
            ;;
        --runs)
            [[ $# -ge 2 ]] || die "--runs requires a value"
            RUNS="$2"
            shift 2
            ;;
        --wax)
            [[ $# -ge 2 ]] || die "--wax requires a path"
            WAX="$2"
            shift 2
            ;;
        --brew)
            [[ $# -ge 2 ]] || die "--brew requires a path"
            BREW="$2"
            shift 2
            ;;
        --no-download-probe)
            DOWNLOAD_PROBE=0
            shift
            ;;
        --curl-timeout)
            [[ $# -ge 2 ]] || die "--curl-timeout requires a value"
            CURL_MAX_TIME="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown option: $1"
            ;;
    esac
done

[[ "$RUNS" =~ ^[0-9]+$ && "$RUNS" -gt 0 ]] || die "--runs must be a positive integer"
[[ "$CURL_MAX_TIME" =~ ^[0-9]+$ && "$CURL_MAX_TIME" -gt 0 ]] || die "--curl-timeout must be a positive integer"
[ -x "$WAX" ] || die "wax not found at $WAX - set --wax /path/to/wax or build with 'cargo build --release'"
command -v "$BREW" &>/dev/null || die "brew not found - install Homebrew/Linuxbrew first"
command -v awk &>/dev/null || die "awk required for float math"

# ---------- helpers -----------------------------------------------------------

timeit() {
    local t status
    local TIMEFORMAT='%3R'
    set +e
    t=$( { time "$@" >/dev/null 2>&1; } 2>&1 )
    status=$?
    set -e
    if [[ $status -ne 0 || -z "$t" || ! "$t" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
        die "benchmark command failed or timing was invalid: $*"
    fi
    echo "$t"
}

avg() {
    awk '
        BEGIN { count = 0; sum = 0 }
        /^[0-9]+([.][0-9]+)?$/ { count++; sum += $1 }
        END {
            if (count == 0) {
                print "0.000"
            } else {
                printf "%.3f\n", sum / count
            }
        }
    ' <<< "$(printf "%s\n" "$@")"
}

is_number() {
    [[ "$1" =~ ^[0-9]+([.][0-9]+)?$ ]]
}

calc() {
    awk "BEGIN { printf \"%.3f\", $* }"
}

speedup() {
    awk -v base="$1" -v candidate="$2" '
        BEGIN {
            number = "^[0-9]+([.][0-9]+)?$"
            if (base !~ number || candidate !~ number || base <= 0 || candidate <= 0) {
                print "N/A"
            } else {
                printf "%.1f\n", base / candidate
            }
        }
    '
}

sub_nonnegative() {
    if ! is_number "$1" || ! is_number "$2"; then
        echo "N/A"
        return
    fi
    awk -v wall="$1" -v dl="$2" '
        BEGIN {
            adjusted = wall - dl
            if (adjusted < 0) adjusted = 0
            printf "%.3f\n", adjusted
        }
    '
}

fmt_time() {
    if is_number "$1"; then
        printf "%ss" "$1"
    else
        printf "%s" "$1"
    fi
}

fmt_bytes() {
    if ! is_number "$1"; then
        printf "%s" "$1"
        return
    fi
    awk -v bytes="$1" 'BEGIN { printf "%.1f MB", bytes / 1048576 }'
}

fmt_speed() {
    if ! is_number "$1" || ! is_number "$2" || awk -v t="$2" 'BEGIN { exit !(t <= 0) }'; then
        printf "N/A"
        return
    fi
    awk -v bytes="$1" -v secs="$2" 'BEGIN { printf "%.1f MB/s", (bytes / 1048576) / secs }'
}

bench() {
    local __out="$1"; shift
    local label="$1"; shift
    local times=()
    for i in $(seq 1 "$RUNS"); do
        local t
        printf "    run %-2s running..." "$i"
        t=$(timeit "$@")
        times+=("$t")
        printf "\r    run %-2s %ss       \n" "$i" "$t"
    done
    local a; a=$(avg "${times[@]}")
    printf "    ${BOLD}avg  %ss${NC}   (%s)\n" "$a" "$label"
    printf -v "$__out" '%s' "$a"
}

system_value() {
    local label="$1"
    local fallback="$2"
    shift 2
    "$@" 2>/dev/null || printf "%s" "$fallback"
}

formula_bottle_url() {
    local name="$1"
    local json
    command -v curl &>/dev/null || return 1
    json="$(curl -fsSL "https://formulae.brew.sh/api/formula/${name}.json" 2>/dev/null)" || return 1

    if command -v ruby &>/dev/null; then
        ruby -rjson -rrbconfig -e '
            data = JSON.parse(STDIN.read)
            files = data.dig("bottle", "stable", "files") || {}
            cpu = RbConfig::CONFIG["host_cpu"]
            arm = cpu.match?(/arm|aarch64/)
            keys = files.keys
            preferred = if RUBY_PLATFORM.include?("darwin")
                arm ? keys.select { |k| k.start_with?("arm64_") } :
                      keys.reject { |k| k.start_with?("arm64_") || k.include?("linux") }
            else
                arm ? keys.select { |k| k.include?("arm64_linux") } :
                      keys.select { |k| k.include?("x86_64_linux") }
            end
            preferred << "all" if files.key?("all")
            preferred.concat(keys)
            entry = preferred.map { |k| files[k] }.compact.find { |v| v["url"] }
            puts entry["url"] if entry
        ' <<< "$json"
    elif command -v python3 &>/dev/null; then
        python3 -c '
import json, platform, sys
data = json.load(sys.stdin)
files = (((data.get("bottle") or {}).get("stable") or {}).get("files") or {})
machine = platform.machine().lower()
arm = "arm" in machine or "aarch64" in machine
keys = list(files.keys())
if sys.platform == "darwin":
    preferred = [k for k in keys if k.startswith("arm64_")] if arm else [k for k in keys if not k.startswith("arm64_") and "linux" not in k]
else:
    preferred = [k for k in keys if "arm64_linux" in k] if arm else [k for k in keys if "x86_64_linux" in k]
if "all" in files:
    preferred.append("all")
preferred += keys
for key in preferred:
    entry = files.get(key) or {}
    url = entry.get("url")
    if url:
        print(url)
        break
' <<< "$json"
    else
        return 1
    fi
}

json_token_field() {
    if command -v ruby &>/dev/null; then
        ruby -rjson -e 'puts JSON.parse(STDIN.read)["token"].to_s'
    elif command -v python3 &>/dev/null; then
        python3 -c 'import json, sys; print((json.load(sys.stdin).get("token") or ""))'
    else
        sed -n 's/.*"token":"\([^"]*\)".*/\1/p'
    fi
}

ghcr_token_for_url() {
    local url="$1"
    local repo token_json
    [[ "$url" =~ ^https://ghcr\.io/v2/(.+)/blobs/ ]] || return 1
    repo="${BASH_REMATCH[1]}"
    token_json="$(curl -fsSL "https://ghcr.io/token?service=ghcr.io&scope=repository:${repo}:pull" 2>/dev/null)" || return 1
    json_token_field <<< "$token_json"
}

measure_download_group() {
    local __time="$1"; shift
    local __bytes="$1"; shift
    local label="$1"; shift
    local total_time="0.000"
    local total_bytes="0"
    local measured=0

    if [[ "$DOWNLOAD_PROBE" != "1" ]]; then
        printf -v "$__time" '%s' "N/A"
        printf -v "$__bytes" '%s' "N/A"
        return
    fi
    command -v curl &>/dev/null || {
        printf -v "$__time" '%s' "N/A"
        printf -v "$__bytes" '%s' "N/A"
        return
    }

    echo -e "\n  ${CYAN}download-only probe: ${label}${NC}"
    for formula in "$@"; do
        local url token stats seconds bytes
        printf "    %-12s resolving bottle..." "$formula"
        url="$(formula_bottle_url "$formula" | head -1 || true)"
        if [[ -z "$url" ]]; then
            printf "\r    %-12s %s              \n" "$formula" "N/A (no bottle URL)"
            continue
        fi
        token="$(ghcr_token_for_url "$url" || true)"
        printf "\r    %-12s downloading..." "$formula"
        if [[ -n "$token" ]]; then
            stats="$(curl -fL --connect-timeout "$CURL_CONNECT_TIMEOUT" --max-time "$CURL_MAX_TIME" -H "Authorization: Bearer ${token}" --output /dev/null --write-out '%{time_total} %{size_download}' "$url" 2>/dev/null || true)"
        else
            stats="$(curl -fL --connect-timeout "$CURL_CONNECT_TIMEOUT" --max-time "$CURL_MAX_TIME" --output /dev/null --write-out '%{time_total} %{size_download}' "$url" 2>/dev/null || true)"
        fi
        seconds="${stats%% *}"
        bytes="${stats##* }"
        if ! is_number "$seconds" || ! is_number "$bytes" || [[ "$bytes" == "0" ]]; then
            printf "\r    %-12s %s              \n" "$formula" "N/A (download failed or timed out)"
            continue
        fi
        total_time="$(calc "$total_time + $seconds")"
        total_bytes="$(calc "$total_bytes + $bytes")"
        measured=1
        printf "\r    %-12s %8s in %7s @ %s              \n" "$formula" "$(fmt_bytes "$bytes")" "$(fmt_time "$seconds")" "$(fmt_speed "$bytes" "$seconds")"
    done

    if [[ "$measured" != "1" ]]; then
        printf -v "$__time" '%s' "N/A"
        printf -v "$__bytes" '%s' "N/A"
        return
    fi
    printf "    %-12s %8s in %7s @ %s\n" "total" "$(fmt_bytes "$total_bytes")" "$(fmt_time "$total_time")" "$(fmt_speed "$total_bytes" "$total_time")"
    printf -v "$__time" '%s' "$total_time"
    printf -v "$__bytes" '%s' "$total_bytes"
}

# ---------- system info -------------------------------------------------------

OS_INFO="$(sw_vers -productName 2>/dev/null || uname -s) $(sw_vers -productVersion 2>/dev/null || uname -r) ($(uname -m))"
HOST_INFO="$(sysctl -n hw.model 2>/dev/null || hostname)"
CPU_INFO="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || grep 'model name' /proc/cpuinfo 2>/dev/null | head -1 | cut -d: -f2- | xargs || echo unknown)"
RAM_INFO="$(sysctl -n hw.memsize 2>/dev/null | awk '{printf "%.0f GiB", $1/1073741824}' || free -h 2>/dev/null | awk '/^Mem/{print $2}' || echo unknown)"
WAX_VERSION="$("$WAX" --version 2>/dev/null | head -1)"
BREW_VERSION="$("$BREW" --version | head -1)"
GIT_COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
RUN_DATE="$(date -u '+%Y-%m-%d %H:%M:%S UTC' 2>/dev/null || date)"
INSTALL_MODE="$([[ "$WITH_INSTALLS" == "1" ]] && echo enabled || echo skipped)"
DOWNLOAD_MODE="$([[ "$DOWNLOAD_PROBE" == "1" ]] && echo enabled || echo skipped)"

echo -e "\n${BOLD}=== System ===${NC}"
printf "  OS:      %s\n" "$OS_INFO"
printf "  Host:    %s\n" "$HOST_INFO"
printf "  CPU:     %s\n" "$CPU_INFO"
printf "  Memory:  %s\n" "$RAM_INFO"
printf "  wax:     %s (%s)\n" "$WAX_VERSION" "$WAX"
printf "  brew:    %s (%s)\n" "$BREW_VERSION" "$BREW"
printf "  runs:    %s per benchmark\n" "$RUNS"
printf "  installs: %s\n" "$INSTALL_MODE"
printf "  download probe: %s\n" "$DOWNLOAD_MODE"
if [[ "$DOWNLOAD_PROBE" == "1" ]]; then
    printf "  download timeout: %ss per bottle\n" "$CURL_MAX_TIME"
fi

# ---------- 1. update ---------------------------------------------------------

echo -e "\n${BOLD}=== 1. Update (index/formula sync) ===${NC}"

echo -e "\n  ${CYAN}wax update (warm cache)${NC}"
"$WAX" update >/dev/null 2>&1
bench wax_update "wax warm" "$WAX" update

echo -e "\n  ${CYAN}brew update (warm cache)${NC}"
"$BREW" update >/dev/null 2>&1
bench brew_update "brew warm" "$BREW" update

echo -e "\n  speedup: ${GREEN}$(speedup "$brew_update" "$wax_update") faster${NC}"

# ---------- 2. search ---------------------------------------------------------

echo -e "\n${BOLD}=== 2. Search (nginx) ===${NC}"

echo -e "\n  ${CYAN}wax search nginx${NC}"
bench wax_search "wax" "$WAX" search nginx

echo -e "\n  ${CYAN}brew search nginx${NC}"
bench brew_search "brew" "$BREW" search nginx

echo -e "\n  speedup: ${GREEN}$(speedup "$brew_search" "$wax_search") faster${NC}"

# ---------- 3. info -----------------------------------------------------------

echo -e "\n${BOLD}=== 3. Info (nginx) ===${NC}"

echo -e "\n  ${CYAN}wax info nginx${NC}"
bench wax_info "wax" "$WAX" info nginx

echo -e "\n  ${CYAN}brew info nginx${NC}"
bench brew_info "brew" "$BREW" info nginx

echo -e "\n  speedup: ${GREEN}$(speedup "$brew_info" "$wax_info") faster${NC}"

if [[ "$WITH_INSTALLS" == "1" ]]; then

# ---------- 4. single-package install ----------------------------------------

echo -e "\n${BOLD}=== 4. Install: tree (single package) ===${NC}"
echo "  install benchmark will uninstall/reinstall tree before each run"

measure_download_group tree_dl_time tree_dl_bytes "tree" tree

wax_tree_times=()
for i in $(seq 1 "$RUNS"); do
    "$WAX" uninstall tree >/dev/null 2>&1 || true
    printf "    wax run %-2s installing..." "$i"
    t=$(timeit "$WAX" install tree --user)
    wax_tree_times+=("$t")
    printf "\r    wax run %-2s %ss       \n" "$i" "$t"
done
wax_tree=$(avg "${wax_tree_times[@]}")
wax_tree_adj=$(sub_nonnegative "$wax_tree" "$tree_dl_time")
printf "    ${BOLD}wax avg   %ss wall, %s download-adjusted${NC}\n" "$wax_tree" "$(fmt_time "$wax_tree_adj")"

brew_tree_times=()
for i in $(seq 1 "$RUNS"); do
    "$BREW" uninstall --force tree >/dev/null 2>&1 || true
    printf "    brew run %-2s installing..." "$i"
    t=$(timeit "$BREW" install tree)
    brew_tree_times+=("$t")
    printf "\r    brew run %-2s %ss       \n" "$i" "$t"
done
brew_tree=$(avg "${brew_tree_times[@]}")
brew_tree_adj=$(sub_nonnegative "$brew_tree" "$tree_dl_time")
printf "    ${BOLD}brew avg  %ss wall, %s download-adjusted${NC}\n" "$brew_tree" "$(fmt_time "$brew_tree_adj")"

echo -e "\n  wall speedup: ${GREEN}$(speedup "$brew_tree" "$wax_tree") faster${NC}"
echo -e "  adjusted speedup: ${GREEN}$(speedup "$brew_tree_adj" "$wax_tree_adj") faster${NC}"

# ---------- 5. multi-package parallel install ---------------------------------

echo -e "\n${BOLD}=== 5. Install: ripgrep + bat + fd (multi-package) ===${NC}"
echo "  install benchmark will uninstall/reinstall ripgrep, bat, and fd before each run"

measure_download_group multi_dl_time multi_dl_bytes "ripgrep + bat + fd" ripgrep bat fd

wax_multi_times=()
for i in $(seq 1 "$RUNS"); do
    "$WAX" uninstall ripgrep bat fd >/dev/null 2>&1 || true
    printf "    wax run %-2s installing..." "$i"
    t=$(timeit "$WAX" install ripgrep bat fd --user)
    wax_multi_times+=("$t")
    printf "\r    wax run %-2s %ss       \n" "$i" "$t"
done
wax_multi=$(avg "${wax_multi_times[@]}")
wax_multi_adj=$(sub_nonnegative "$wax_multi" "$multi_dl_time")
printf "    ${BOLD}wax avg   %ss wall, %s download-adjusted${NC}\n" "$wax_multi" "$(fmt_time "$wax_multi_adj")"

brew_multi_times=()
for i in $(seq 1 "$RUNS"); do
    "$BREW" uninstall --force ripgrep bat fd >/dev/null 2>&1 || true
    printf "    brew run %-2s installing..." "$i"
    t=$(timeit "$BREW" install ripgrep bat fd)
    brew_multi_times+=("$t")
    printf "\r    brew run %-2s %ss       \n" "$i" "$t"
done
brew_multi=$(avg "${brew_multi_times[@]}")
brew_multi_adj=$(sub_nonnegative "$brew_multi" "$multi_dl_time")
printf "    ${BOLD}brew avg  %ss wall, %s download-adjusted${NC}\n" "$brew_multi" "$(fmt_time "$brew_multi_adj")"

echo -e "\n  wall speedup: ${GREEN}$(speedup "$brew_multi" "$wax_multi") faster${NC}"
echo -e "  adjusted speedup: ${GREEN}$(speedup "$brew_multi_adj" "$wax_multi_adj") faster${NC}"

else
    echo -e "\n${BOLD}=== 4-5. Install benchmarks skipped ===${NC}"
    echo "  Pass --with-installs to allow uninstall/reinstall benchmarks."
    tree_dl_time="N/A"; tree_dl_bytes="N/A"
    multi_dl_time="N/A"; multi_dl_bytes="N/A"
    wax_tree="skipped"; brew_tree="skipped"; wax_tree_adj="skipped"; brew_tree_adj="skipped"
    wax_multi="skipped"; brew_multi="skipped"; wax_multi_adj="skipped"; brew_multi_adj="skipped"
fi

# ---------- summary -----------------------------------------------------------

echo -e "\n${BOLD}=== Summary ===${NC}"
printf "\n  %-24s %10s %10s %10s\n" "Benchmark" "wax" "brew" "speedup"
printf  "  %-24s %10s %10s %10s\n" "---------" "---" "----" "-------"
printf  "  %-24s %10s %10s %10s\n" "update (warm)" "$(fmt_time "$wax_update")" "$(fmt_time "$brew_update")" "$(speedup "$brew_update" "$wax_update")"
printf  "  %-24s %10s %10s %10s\n" "search nginx" "$(fmt_time "$wax_search")" "$(fmt_time "$brew_search")" "$(speedup "$brew_search" "$wax_search")"
printf  "  %-24s %10s %10s %10s\n" "info nginx" "$(fmt_time "$wax_info")" "$(fmt_time "$brew_info")" "$(speedup "$brew_info" "$wax_info")"
printf  "  %-24s %10s %10s %10s\n" "install tree wall" "$(fmt_time "$wax_tree")" "$(fmt_time "$brew_tree")" "$(speedup "$brew_tree" "$wax_tree")"
printf  "  %-24s %10s %10s %10s\n" "install tree - dl" "$(fmt_time "$wax_tree_adj")" "$(fmt_time "$brew_tree_adj")" "$(speedup "$brew_tree_adj" "$wax_tree_adj")"
printf  "  %-24s %10s %10s %10s\n" "multi install wall" "$(fmt_time "$wax_multi")" "$(fmt_time "$brew_multi")" "$(speedup "$brew_multi" "$wax_multi")"
printf  "  %-24s %10s %10s %10s\n" "multi install - dl" "$(fmt_time "$wax_multi_adj")" "$(fmt_time "$brew_multi_adj")" "$(speedup "$brew_multi_adj" "$wax_multi_adj")"

echo -e "\n${BOLD}=== Copyable Report ===${NC}"
cat <<EOF
wax benchmark report
date: $RUN_DATE
git_commit: $GIT_COMMIT
os: $OS_INFO
host: $HOST_INFO
cpu: $CPU_INFO
memory: $RAM_INFO
wax: $WAX_VERSION ($WAX)
brew: $BREW_VERSION ($BREW)
runs: $RUNS
install_benchmarks: $INSTALL_MODE
download_probe: $DOWNLOAD_MODE
download_timeout_seconds: $([[ "$DOWNLOAD_PROBE" == "1" ]] && echo "$CURL_MAX_TIME" || echo "N/A")
download_tree: $(fmt_bytes "$tree_dl_bytes") in $(fmt_time "$tree_dl_time") @ $(fmt_speed "$tree_dl_bytes" "$tree_dl_time")
download_ripgrep_bat_fd: $(fmt_bytes "$multi_dl_bytes") in $(fmt_time "$multi_dl_time") @ $(fmt_speed "$multi_dl_bytes" "$multi_dl_time")

results:
  update_warm: wax $(fmt_time "$wax_update"), brew $(fmt_time "$brew_update"), speedup $(speedup "$brew_update" "$wax_update")
  search_nginx: wax $(fmt_time "$wax_search"), brew $(fmt_time "$brew_search"), speedup $(speedup "$brew_search" "$wax_search")
  info_nginx: wax $(fmt_time "$wax_info"), brew $(fmt_time "$brew_info"), speedup $(speedup "$brew_info" "$wax_info")
  install_tree_wall: wax $(fmt_time "$wax_tree"), brew $(fmt_time "$brew_tree"), speedup $(speedup "$brew_tree" "$wax_tree")
  install_tree_download_adjusted: wax $(fmt_time "$wax_tree_adj"), brew $(fmt_time "$brew_tree_adj"), speedup $(speedup "$brew_tree_adj" "$wax_tree_adj")
  install_ripgrep_bat_fd_wall: wax $(fmt_time "$wax_multi"), brew $(fmt_time "$brew_multi"), speedup $(speedup "$brew_multi" "$wax_multi")
  install_ripgrep_bat_fd_download_adjusted: wax $(fmt_time "$wax_multi_adj"), brew $(fmt_time "$brew_multi_adj"), speedup $(speedup "$brew_multi_adj" "$wax_multi_adj")
EOF
