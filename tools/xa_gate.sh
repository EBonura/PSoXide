#!/bin/sh
# Headless gate for XA-ADPCM music: play hello-xa in the emulator, press CROSS
# three times, and check from the audio capture that the four songs came out
# in order (their tones are known) and from the drive command log that the
# player filtered on channels 0, 1, 2 and 3.
#
# Run as: sh tools/xa_gate.sh <frontend> <hello-xa.cue> <psx-audio-cook>
# Build the disc with `make hello-xa-disc`; the frontend is the headless
# PSoXide emulator, and psx-audio-cook comes from `cargo build --release -p psx-audio-cook`.
set -eu
FRONTEND="${1:?usage: xa_gate.sh <frontend> <hello-xa.cue> <psx-audio-cook>}"
CUE="${2:?missing cue}"
COOK="${3:?missing psx-audio-cook}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$FRONTEND" launch --path "$CUE" --steps 170000000 \
    --pad-pulses '0x4000@220+6,0x4000@440+6,0x4000@560+6' \
    --dump-audio "$WORK/capture.wav" --cd-command-log "$WORK/cd.csv" >/dev/null

# Setfilter (0x0D) parameters, file 1: channels 0, 1, 2, 3 in order.
FILTERS="$(awk -F, '$2 == "0x0D" { printf "%s;", $4 }' "$WORK/cd.csv")"
[ "$FILTERS" = "01 00;01 01;01 02;01 03;" ] || { echo "xa-gate: FAIL: Setfilter sequence '$FILTERS'" >&2; exit 1; }

# Which song each half second of audio is: the pad (a chord near A), the high
# chord near G, the 880 Hz blips and the whistle sweep. Silent windows (a loop
# restart) and mixed ones are skipped.
SONGS="$("$COOK" xa-peaks "$WORK/capture.wav" | awk -F'\t' '
    BEGIN { last = -1 }
    $2 < 300 { next }
    $3 >= 200 && $3 <= 360  { s = 0 }
    $3 >= 370 && $3 <= 620  { s = 1 }
    $3 >= 860 && $3 <= 900  { s = 2 }
    $3 >= 950 && $3 <= 1900 { s = 3 }
    { if (s != last) { out = out s " "; last = s } }
    END { print out }')"
[ "$SONGS" = "0 1 2 3 " ] || { echo "xa-gate: FAIL: songs heard '$SONGS', wanted '0 1 2 3 '" >&2; exit 1; }
echo "xa-gate: PASS (filters $FILTERS songs $SONGS)"
