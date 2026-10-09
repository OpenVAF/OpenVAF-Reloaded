#!/bin/bash
# Compile and run every transient regression case, and report pass/fail.
#
# Local only: ngspice is not a build dependency, so this is not wired into
# `cargo test`. Point NGSPICE at your binary if it is not on PATH.
set -u

here="$(cd "$(dirname "$0")" && pwd)"
OV="${OPENVAF:-$here/../target/release/openvaf-r}"
NG="${NGSPICE:-ngspice}"

if [ ! -x "$OV" ]; then
    echo "no compiler at $OV -- build it first, or set OPENVAF" >&2
    exit 2
fi
if ! command -v "$NG" >/dev/null 2>&1; then
    echo "ngspice not found as '$NG' -- set NGSPICE to its path" >&2
    exit 2
fi

fail=0

run() {
    dir="$1"; model="$2"; out="$3"; analyzer="$4"
    cd "$here/$dir" || return 1

    if ! "$OV" "$model.va" -o "$model.osdi" >/dev/null 2>&1; then
        printf '%-14s COMPILE FAILED\n' "$dir"
        "$OV" "$model.va" -o "$model.osdi"
        fail=1
        return
    fi

    # ngspice's console output goes to its own file: `wrdata` is already writing
    # "$out", and letting both land in one file interleaves the log into the data.
    "$NG" -b "$model.cir" > "$model.log" 2>&1

    if log=$(python3 "../$analyzer" 2>&1); then
        printf '%-14s PASS\n' "$dir"
    else
        printf '%-14s FAIL\n' "$dir"
        printf '%s\n' "$log" | sed 's/^/  /'
        fail=1
    fi
}

run sample_hold   sample_hold   sh_out.txt analyze_sample_hold.py
run above_init    above_init    ai_out.txt analyze_above_init.py
run last_crossing last_crossing lc_out.txt analyze_last_crossing.py
run timer         timer         tm_out.txt analyze_timer.py
run transition    trfilter      tr_out.txt analyze_transition.py

exit $fail
