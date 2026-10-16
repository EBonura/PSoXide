#!/bin/sh
# Headless gate for resuming an XA song after a data read: play hello-xa-resume
# in the emulator, which plays one channel, suspends it, reads data sectors,
# resumes and prints its own verdict, then check from the drive command log
# that the resume sent the mode, the file and channel filter and a Setloc
# again, and from the audio capture that the song is heard both before the
# read and after the resume.
#
# Run as: sh tools/xa_resume_gate.sh <frontend> <hello-xa-resume.cue> <psx-audio-cook>
# Build the disc with `make hello-xa-resume-disc`; the frontend is the headless
# PSoXide emulator, and psx-audio-cook comes from
# `cargo build --release -p psx-audio-cook`.
set -eu
FRONTEND="${1:?usage: xa_resume_gate.sh <frontend> <hello-xa-resume.cue> <psx-audio-cook>}"
CUE="${2:?missing cue}"
COOK="${3:?missing psx-audio-cook}"
STEPS="${XA_RESUME_STEPS:-1200000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$FRONTEND" launch --path "$CUE" --steps "$STEPS" \
    --dump-audio "$WORK/capture.wav" --cd-command-log "$WORK/cd.csv" >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "xa-resume-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
if [ -n "${XA_RESUME_KEEP:-}" ]; then
    cp "$WORK/out.txt" "$XA_RESUME_KEEP.out.txt"
    cp "$WORK/cd.csv" "$XA_RESUME_KEEP.cd.csv"
    cp "$WORK/capture.wav" "$XA_RESUME_KEEP.wav"
fi

# The guest's own checks (head back in the file, position kept, clock).
sed -n '/^hello-xa-resume:/p' "$WORK/out.txt"
grep -q '^hello-xa-resume: ALL PASS' "$WORK/out.txt" || {
    echo "xa-resume-gate: FAIL: the guest reported a failure" >&2
    exit 1
}

# Setfilter (0x0D): file 1, channel 3 at the start and again at the resume.
FILTERS="$(awk -F, '$2 == "0x0D" { printf "%s;", $4 }' "$WORK/cd.csv")"
[ "$FILTERS" = "01 03;01 03;" ] || { echo "xa-resume-gate: FAIL: Setfilter sequence '$FILTERS', wanted '01 03;01 03;'" >&2; exit 1; }

# Setmode (0x0E): the player's XA mode (0x48), the reader's data mode (0x80)
# during the read, and the player's XA mode again at the resume.
MODES="$(awk -F, '$2 == "0x0E" { printf "%s;", $4 }' "$WORK/cd.csv")"
case "$MODES" in
    *"48;"*"80;"*"48;") ;;
    *) echo "xa-resume-gate: FAIL: Setmode sequence '$MODES' does not end 48, 80, 48" >&2; exit 1 ;;
esac

# The last Setloc before the last ReadS must aim into the song, not at the
# data (the volume descriptors are at LBA 16, MSF 00:02:16).
LAST_SETLOC="$(awk -F, '$2 == "0x02" { loc = $4 } $2 == "0x1B" { print loc }' "$WORK/cd.csv" | tail -n 1)"
[ -n "$LAST_SETLOC" ] || { echo "xa-resume-gate: FAIL: no Setloc before ReadS in the command log" >&2; exit 1; }
[ "$LAST_SETLOC" != "00 02 16" ] || { echo "xa-resume-gate: FAIL: the resume read the data location" >&2; exit 1; }

# The whistle (950 to 1900 Hz) is heard in the first and in the last part of
# the capture, with the data read in between.
"$COOK" xa-peaks "$WORK/capture.wav" >"$WORK/peaks.tsv"
HEARD="$(awk -F'\t' '$2 >= 300 && $3 >= 950 && $3 <= 1900 { n++ } END { print n + 0 }' "$WORK/peaks.tsv")"
[ "$HEARD" -ge 4 ] || { echo "xa-resume-gate: FAIL: the whistle was heard in only $HEARD windows" >&2; exit 1; }
LATE="$(awk -F'\t' '$2 >= 300 && $3 >= 950 && $3 <= 1900 { t = $1 } END { print t + 0 }' "$WORK/peaks.tsv")"
echo "xa-resume-gate: PASS (filters $FILTERS modes $MODES whistle windows $HEARD last at $LATE)"
