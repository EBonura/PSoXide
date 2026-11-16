#!/bin/sh
# Headless gate for the XA player's non-blocking stop: play hello-xa in the emulator, stop the
# song with CIRCLE, start the next with CROSS, then stop with CIRCLE and ask for another song
# with CROSS while the Pause is still out. Checks, from the route log, the drive command log
# and the audio capture, that
#
#   1. the pad is polled and frames are presented without a gap longer than one vblank over
#      normal (hello-xa polls once per presented frame, every second VBlank under the 30 fps
#      cap, so a gap above 3 route ticks is a stall; a blocking Pause out of a read is 8);
#   2. the song is silent within 60 ms of each stop's Pause and stays silent until the next
#      song starts (no tail while the drive finishes pausing);
#   3. the song asked for while a stop was out waits for the Pause to complete: the first
#      command after that Pause is at least 1.2 million clocks later (the SDK's documented
#      earliest completion) and is the Demute of the new song's start;
#   4. the songs heard, in order, are the three that were asked for.
#
# Run as: sh tools/xa_stop_gate.sh <frontend> <hello-xa.cue> <psx-audio-cook>
# Build the disc with `make hello-xa-disc`; the frontend is the headless PSoXide emulator, and
# psx-audio-cook comes from `cargo build --release -p psx-audio-cook`. The hardware target is
# the frontend's (`PSOXIDE_HARDWARE=mister`, ...).
set -eu
FRONTEND="${1:?usage: xa_stop_gate.sh <frontend> <hello-xa.cue> <psx-audio-cook>}"
CUE="${2:?missing cue}"
COOK="${3:?missing psx-audio-cook}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# CIRCLE (0x2000) stops, CROSS (0x4000) plays the next song. The pulses are on the route
# clock (one tick a VBlank): stop at 300, next song at 500, stop at 700 and next song six
# ticks later, three polls on, while the second Pause is still out (it completes about eight
# VBlanks after it is sent).
PULSES='0x2000@300+3,0x4000@500+3,0x2000@700+3,0x4000@706+3'
"$FRONTEND" launch --path "$CUE" --steps "${XA_GATE_STEP_CEILING:-3000000000}" \
    --stop-at-poll "${XA_GATE_STOP_POLL:-480}" --pad-pulses "$PULSES" \
    --dump-audio "$WORK/capture.wav" --cd-command-log "$WORK/cd.csv" \
    --route-log "$WORK/route.csv" >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "xa-stop-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
grep -q 'stopped-at=' "$WORK/out.txt" || {
    echo "xa-stop-gate: FAIL: the poll limit was never reached within the step ceiling" >&2
    exit 1
}

# 1. Gaps between pad polls and between display flips, from tick 250 on (the boot is over).
GAPS="$(awk -F, '
    NR == 1 { next }
    $1 < 250 { lp = $12; lpt = $1; lft = $1; next }
    { if ($12 != lp) { if ($1 - lpt > maxp) maxp = $1 - lpt; lp = $12; lpt = $1 }
      if ($11 > 0)   { if ($1 - lft > maxf) maxf = $1 - lft; lft = $1 } }
    END { print maxp + 0, maxf + 0 }' "$WORK/route.csv")"
MAX_POLL_GAP="${GAPS% *}"
MAX_FLIP_GAP="${GAPS#* }"
[ "$MAX_POLL_GAP" -le 3 ] && [ "$MAX_FLIP_GAP" -le 3 ] || {
    echo "xa-stop-gate: FAIL: longest pad gap $MAX_POLL_GAP, longest flip gap $MAX_FLIP_GAP VBlanks (limit 3)" >&2
    exit 1
}

# Setfilter channels: the three songs, in order.
FILTERS="$(awk -F, '$2 == "0x0D" { printf "%s;", $4 }' "$WORK/cd.csv")"
[ "$FILTERS" = "01 00;01 01;01 02;" ] || { echo "xa-stop-gate: FAIL: Setfilter sequence '$FILTERS'" >&2; exit 1; }

# The stops: the Pauses after the first ReadS (the boot sends two before any song).
awk -F, '$2 == "0x1B" { seen = 1 } seen && $2 == "0x09" { print NR ":" $1 }' "$WORK/cd.csv" >"$WORK/pauses.txt"
[ "$(wc -l <"$WORK/pauses.txt")" -eq 2 ] || { echo "xa-stop-gate: FAIL: expected 2 stop Pauses, saw $(wc -l <"$WORK/pauses.txt")" >&2; exit 1; }

# 3. Nothing is sent while the second Pause is out, and what follows it is the new song's Demute.
ROW="$(sed -n 2p "$WORK/pauses.txt" | cut -d: -f1)"
AFTER="$(awk -F, -v row="$ROW" 'NR == row { at = $1 } NR == row + 1 { print $1 - at, $2 }' "$WORK/cd.csv")"
GAP_CLOCKS="${AFTER% *}"
NEXT_COMMAND="${AFTER#* }"
{ [ "$GAP_CLOCKS" -ge 1200000 ] && [ "$NEXT_COMMAND" = "0x0C" ]; } || {
    echo "xa-stop-gate: FAIL: after the second stop's Pause the next command is $NEXT_COMMAND after $GAP_CLOCKS clocks (wanted Demute after 1200000 or more)" >&2
    exit 1
}

# 2. Silence within 60 ms of each stop's Pause, from the capture (10 ms windows of 16-bit
# stereo at 44.1 kHz; a window is silent when no sample reaches 64).
od -An -v -t d2 -j 44 "$WORK/capture.wav" | awk '
    { for (i = 1; i <= NF; i++) { v = $i < 0 ? -$i : $i; if (v > m) m = v; k++
        if (k % 2 == 0) { n++
          if (n % 441 == 0) { win++; peak[win] = m; m = 0 } } } }
    END { for (i = 1; i <= win; i++) print (i - 1) * 0.01, peak[i] }' >"$WORK/windows.txt"
while IFS=: read -r _ clocks; do
    START="$(awk -v c="$clocks" 'BEGIN { printf "%.3f", c / 33868800 }')"
    # The first silent window starting within 60 ms after the Pause, and 100 ms of silence from it.
    OK="$(awk -v s="$START" '
        $1 >= s - 0.01 && !from && $2 < 64 { from = $1 }
        from && $1 >= from && $1 < from + 0.1 { total++; if ($2 < 64) quiet++ }
        END { print (from != "" && from <= s + 0.06 && total >= 10 && quiet == total) ? "yes" : "no " from }' "$WORK/windows.txt")"
    [ "$OK" = "yes" ] || { echo "xa-stop-gate: FAIL: the song is not silent within 60 ms of the Pause at $START s ($OK)" >&2; exit 1; }
done <"$WORK/pauses.txt"

# 4. Which song each half second of audio is (the pad near A, the 880 Hz blips; the second song
# is the high chord): silent and mixed windows are skipped.
SONGS="$("$COOK" xa-peaks "$WORK/capture.wav" | awk -F'\t' '
    BEGIN { last = -1 }
    $2 < 300 { next }
    $3 >= 200 && $3 <= 360  { s = 0 }
    $3 >= 370 && $3 <= 620  { s = 1 }
    $3 >= 860 && $3 <= 900  { s = 2 }
    $3 >= 950 && $3 <= 1900 { s = 3 }
    { if (s != last) { out = out s " "; last = s } }
    END { print out }')"
[ "$SONGS" = "0 1 2 " ] || { echo "xa-stop-gate: FAIL: songs heard '$SONGS', wanted '0 1 2 '" >&2; exit 1; }
echo "xa-stop-gate: PASS (longest pad gap $MAX_POLL_GAP, flip gap $MAX_FLIP_GAP VBlanks; queued song $GAP_CLOCKS clocks after its Pause; songs $SONGS)"
