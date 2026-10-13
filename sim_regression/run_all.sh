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

# absdelay is the one case with two realizations to compare (VAMS-2023 4.5.7).
# The in-model one runs anywhere; the descriptor protocol needs a simulator that
# implements `OsdiAbsDelayInfo`, so point NGSPICE_ABSDELAY at a patched binary
# (patches/ngspice-absdelay-history.patch) to have that half checked too.
run_absdelay() {
    dir=absdelay
    cd "$here/$dir" || return 1
    rm -f ad_out.txt ad_proto_out.txt

    for mode in "" "--absdelay in-model"; do
        out=addelay.osdi
        [ -n "$mode" ] && out=addelay_in_model.osdi
        if ! "$OV" $mode "addelay.va" -o "$out" >/dev/null 2>&1; then
            printf '%-14s COMPILE FAILED\n' "$dir"
            "$OV" $mode "addelay.va" -o "$out"
            fail=1
            return
        fi
    done

    "$NG" -b addelay_in_model.cir > addelay_in_model.log 2>&1
    if [ -n "${NGSPICE_ABSDELAY:-}" ]; then
        "$NGSPICE_ABSDELAY" -b addelay.cir > addelay.log 2>&1
    fi

    if log=$(python3 "../analyze_absdelay.py" 2>&1); then
        printf '%-14s PASS\n' "$dir"
        printf '%s\n' "$log" | sed 's/^/  /'
    else
        printf '%-14s FAIL\n' "$dir"
        printf '%s\n' "$log" | sed 's/^/  /'
        fail=1
    fi
}

# The PLL is the one case that is a whole system rather than one operator: a
# reference that schedules its own edges, a phase detector on `cross`, a charge
# pump, a divider and an oscillator, closed into a loop that has to lock. It takes
# longer than the rest of the suite together and its numbers are worth reading
# whether or not it passed, so they are always printed.
# A vector source that streams its file, a record at a time (VAMS-2023 9.5.4).
# The numbers are worth seeing on a pass, so this one prints them, and it runs
# under both binaries when there is a second: the stream has to keep its place
# whether the simulator confirms its timepoints or the model has to infer it.
run_vecstream() {
    dir=vecstream
    cd "$here/$dir" || return 1

    if ! "$OV" vecstream.va -o vecstream.osdi >/dev/null 2>&1; then
        printf '%-14s COMPILE FAILED\n' "$dir"
        "$OV" vecstream.va -o vecstream.osdi
        fail=1
        return
    fi

    ok=1
    out=""
    for ng in "$NG" ${NGSPICE_TIMEPOINTS:+"$NGSPICE_TIMEPOINTS"}; do
        rm -f vstream_out.txt
        "$ng" -b vecstream.cir > vecstream.log 2>&1
        if log=$(python3 "../analyze_vecstream.py" 2>&1); then
            out="$out$log
"
        else
            out="$out$log
"
            ok=0
        fi
    done

    if [ "$ok" = 1 ]; then
        printf '%-14s PASS\n' "$dir"
    else
        printf '%-14s FAIL\n' "$dir"
        fail=1
    fi
    printf '%s' "$out" | sed 's/^/  /'
}

run_pll() {
    dir=pll
    cd "$here/$dir" || return 1

    if ! "$OV" pll.va -o pll.osdi >/dev/null 2>&1; then
        printf '%-14s COMPILE FAILED\n' "$dir"
        "$OV" pll.va -o pll.osdi
        fail=1
        return
    fi

    "$NG" -b pll.cir > pll.log 2>&1

    if log=$(python3 "../analyze_pll.py" 2>&1); then
        printf '%-14s PASS\n' "$dir"
        printf '%s\n' "$log" | sed 's/^/  /'
    else
        printf '%-14s FAIL\n' "$dir"
        printf '%s\n' "$log" | sed 's/^/  /'
        fail=1
    fi
}

# The timepoint protocol: the same switch announcing its jump and not announcing
# it, which has to be two runs because the integration order belongs to the
# circuit. A simulator that does not read the protocol has to produce the same
# waveform either way; one that does is pointed at by NGSPICE_TIMEPOINTS
# (patches/ngspice-osdi-timepoints.patch) and gets a second pair of runs.
run_timepoints() {
    dir=timepoints
    cd "$here/$dir" || return 1
    rm -f tp_on.txt tp_off.txt tp_proto_on.txt tp_proto_off.txt

    if ! "$OV" jump.va -o jump.osdi >/dev/null 2>&1; then
        printf '%-14s COMPILE FAILED\n' "$dir"
        "$OV" jump.va -o jump.osdi
        fail=1
        return
    fi

    "$NG" -b jump.cir > jump.log 2>&1
    "$NG" -b jump_off.cir > jump_off.log 2>&1

    if [ -n "${NGSPICE_TIMEPOINTS:-}" ]; then
        sed -e 's/tp_on.txt/tp_proto_on.txt/' jump.cir > jump_proto.cir
        sed -e 's/tp_off.txt/tp_proto_off.txt/' jump_off.cir > jump_proto_off.cir
        OSDI_TIMEPOINT_BREAKPOINTS=0 "$NGSPICE_TIMEPOINTS" -b jump_proto.cir \
            > jump_proto.log 2>&1
        OSDI_TIMEPOINT_BREAKPOINTS=0 "$NGSPICE_TIMEPOINTS" -b jump_proto_off.cir \
            > jump_proto_off.log 2>&1
    fi

    if log=$(python3 "../analyze_timepoints.py" 2>&1); then
        printf '%-14s PASS\n' "$dir"
        printf '%s\n' "$log" | sed 's/^/  /'
    else
        printf '%-14s FAIL\n' "$dir"
        printf '%s\n' "$log" | sed 's/^/  /'
        fail=1
    fi
}

# The one case with no ngspice output file at all: the model writes its own
# measurements with 9.5's file tasks, and the analyzer reads those.
run_filelog() {
    dir=filelog
    cd "$here/$dir" || return 1
    rm -f periods.txt evals.txt both_a.txt both_b.txt periods_proto.txt

    if ! "$OV" filelog.va -o filelog.osdi >/dev/null 2>&1; then
        printf '%-14s COMPILE FAILED\n' "$dir"
        "$OV" filelog.va -o filelog.osdi
        fail=1
        return
    fi

    # 9.4.1 through the accept callback, when there is a simulator that makes one:
    # the same model, run first by that one and then by the plain one, so that the
    # analyzer can compare what each was told.
    if [ -n "${NGSPICE_TIMEPOINTS:-}" ]; then
        "$NGSPICE_TIMEPOINTS" -b filelog.cir > filelog_proto.log 2>&1
        mv -f periods.txt periods_proto.txt
        rm -f evals.txt both_a.txt both_b.txt
    fi

    "$NG" -b filelog.cir > filelog.log 2>&1

    if log=$(python3 "../analyze_filelog.py" 2>&1); then
        printf '%-14s PASS\n' "$dir"
        printf '%s\n' "$log" | sed 's/^/  /'
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
run vecsrc        vecsrc        vs_out.txt analyze_vecsrc.py
run global_events global_events ge_out.txt analyze_global_events.py
run directives    directives    dir_out.txt analyze_directives.py
run multidim      xtalk         xt_out.txt analyze_multidim.py
run arrayparam    arrayparam    ap_out.txt analyze_arrayparam.py
run noisetab      noisetab      nt_def.txt analyze_noisetab.py
run carddata      carddata      cd_out.txt analyze_carddata.py
run paramset      paramset      ps_out.txt analyze_paramset.py
run_absdelay
run_timepoints
run_filelog
run_vecstream
run_pll

exit $fail
