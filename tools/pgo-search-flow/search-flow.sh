#!/bin/bash
# search-flow.sh: produce a `psoxide-pgo search` layout for one game, rebuild the ship link with
# it, and prove the ship map follows it. Host only: it runs the game's own build commands and its
# pinned psoxide-pgo (.psoxide/tools/psoxide-pgo), and changes nothing in the game repository but
# the layout file the config names. README.md beside this file says what each step does and how a
# game plugs in.
#
#   search-flow.sh --config FILE --repo DIR --work DIR --frontend PATH
#                  [--from STEP] [--reseed] [--threads N] [--plan]
#
#   STEP is one of: first seed trace search ship check (default: first).
#   --reseed   run the config's SEED_BUILD even when the committed layout binds to the first link.
#   --plan     print the resolved config and the commands each step would run; run nothing.
#
# Environment (optional):
#   FLOW_EMU_WRAP  a command every emulator run is prefixed with, e.g. a machine slot wrapper
#                  ("perl /path/slot.pl"). Default: none.
#   FLOW_PGO       how to run psoxide-pgo. Default: the game's pinned copy,
#                  cargo run -q --release --locked --manifest-path REPO/.psoxide/tools/psoxide-pgo/Cargo.toml --
#
# Exit 0 only when the layout carries the search stamp for the configured rounds and seed, the
# ship build linked with it, and `psoxide-pgo followed` proves the ship map puts every ordered
# function in order with at most MAX_DRIFT placed functions away from the searched address.
set -euo pipefail

die() { echo "search-flow: FAIL: $*" >&2; exit 1; }
say() { echo "search-flow: $*"; }
step() { echo; echo "== $1 $(date '+%F %T')"; }

config= repo= work= frontend= from=first reseed=0 threads= plan=0
while [ $# -gt 0 ]; do
  case $1 in
    --config) config=${2:?}; shift 2 ;;
    --repo) repo=${2:?}; shift 2 ;;
    --work) work=${2:?}; shift 2 ;;
    --frontend) frontend=${2:?}; shift 2 ;;
    --from) from=${2:?}; shift 2 ;;
    --reseed) reseed=1; shift ;;
    --threads) threads=${2:?}; shift 2 ;;
    --plan) plan=1; shift ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) die "unknown argument $1 (see --help)" ;;
  esac
done
[ -n "$config" ] && [ -n "$repo" ] && [ -n "$work" ] && [ -n "$frontend" ] \
  || die "--config, --repo, --work and --frontend are required (see --help)"
[ -f "$config" ] || die "no config at $config"
repo=$(cd "$repo" && pwd -P) || die "no repo at $repo"
mkdir -p "$work"; work=$(cd "$work" && pwd -P)
case $work in "$repo"|"$repo"/*) die "--work must be outside the repository (the harness would see it as a dirty tree)" ;; esac
[ "$plan" = 1 ] || [ -x "$frontend" ] || die "frontend $frontend is not executable"

# ---- config -------------------------------------------------------------------------------
GAME= LAYOUT= LINKER_SCRIPT= DISC_NAME=
FIRST_BUILD= FIRST_EXE= FIRST_MAP= FIRST_DISC=
SHIP_BUILD= SHIP_EXE= SHIP_MAP= SHIP_DISC=
SEED_BUILD= LAUNCH_ARGS= WINDOWS= WEIGHTS=
ROUNDS=600 SEED=1 BATCH=24 THREADS=4 START=greedy MAX_DRIFT=0 MIN_ROUNDS=600 STEPS=40000000000
# shellcheck disable=SC1090
. "$config"
[ -n "$threads" ] && THREADS=$threads
for v in GAME LAYOUT LINKER_SCRIPT FIRST_BUILD FIRST_EXE FIRST_MAP FIRST_DISC \
         SHIP_BUILD SHIP_EXE SHIP_MAP SHIP_DISC WINDOWS; do
  [ -n "${!v}" ] || die "config $config sets no $v"
done
# No early stop: the search runs every round it is given, and a short search is refused here.
# A 1-round search writes a valid stamp that the install check accepts, while its placement can
# measure no better than no layout at all.
[ "$ROUNDS" -ge "$MIN_ROUNDS" ] 2>/dev/null \
  || die "ROUNDS=$ROUNDS is under MIN_ROUNDS=$MIN_ROUNDS (the psoxide-pgo README's recipe is 600)"
PGO=${FLOW_PGO:-cargo run -q --release --locked --manifest-path "$repo/.psoxide/tools/psoxide-pgo/Cargo.toml" --}
WRAP=${FLOW_EMU_WRAP:-}
export FLOW_FRONTEND=$frontend FLOW_REPO=$repo FLOW_WORK=$work

steps="first seed trace search ship check"
case " $steps " in *" $from "*) ;; *) die "--from wants one of: $steps" ;; esac
runs() { # runs STEP: whether STEP is at or after --from
  local s; for s in $steps; do [ "$s" = "$from" ] && return 0; [ "$s" = "$1" ] && return 1; done; return 1
}

pgo() { eval "$PGO" '"$@"' < /dev/null; }
emu() { eval "$WRAP" '"$frontend" launch "$@"' < /dev/null; }

# The newest file matching a repo-relative glob that is newer than a marker.
newest() { # newest GLOB MARKER
  local best= f
  for f in $repo/$1; do
    [ -f "$f" ] && [ "$f" -nt "$2" ] || continue
    if [ -z "$best" ] || [ "$f" -nt "$best" ]; then best=$f; fi
  done
  [ -n "$best" ] || die "the build wrote nothing matching $1 (newer than its start)"
  echo "$best"
}

# Run a build command in the repo with FLOW_OUT set, then copy exe, map and disc into DIR.
build_into() { # build_into NAME CMD EXE MAP DISC DIR
  local name=$1 cmd=$2 exe=$3 map=$4 disc=$5 dir=$6 marker
  rm -rf "$dir"; mkdir -p "$dir/out"
  marker=$dir/.started; : > "$marker"; sleep 1
  export FLOW_OUT=$dir/out
  say "$name build: $cmd"
  (cd "$repo" && eval "$cmd") > "$dir/build.log" 2>&1 || { tail -40 "$dir/build.log" >&2; die "$name build failed (log $dir/build.log)"; }
  cp -c "$(newest "$exe" "$marker")" "$dir/image.exe"
  cp -c "$(newest "$map" "$marker")" "$dir/link.map"
  local cue; cue=$(cd "$repo" && eval "echo $disc")
  case $cue in /*) ;; *) cue=$repo/$cue ;; esac
  [ -f "$cue" ] || die "$name build left no disc at $cue"
  [ "$cue" -nt "$marker" ] || die "$name disc $cue is older than the build (stale disc)"
  cp -c "$cue" "$dir/disc.cue"
  # The cue names its bin by file name: keep that name beside it.
  local bin; bin=$(sed -n 's/^FILE "\(.*\)" BINARY.*/\1/p' "$cue" | head -1)
  [ -n "$bin" ] || die "$cue names no BINARY file"
  cp -c "$(dirname "$cue")/$bin" "$dir/$bin"
  say "$name: exe $(shasum -a 256 "$dir/image.exe" | cut -c1-12), map $(shasum -a 256 "$dir/link.map" | cut -c1-12), disc $bin"
}

layout_path=$repo/$LAYOUT
script_path=$repo/$LINKER_SCRIPT

if [ "$plan" = 1 ]; then
  echo "game          $GAME"
  echo "repo          $repo ($(git -C "$repo" rev-parse --short=12 HEAD 2>/dev/null || echo 'not git'))"
  echo "work          $work"
  echo "layout        $layout_path $([ -f "$layout_path" ] && head -2 "$layout_path" | tail -1 || echo '(none yet)')"
  echo "linker script $script_path $([ -f "$script_path" ] || echo '(MISSING)')"
  echo "first build   $FIRST_BUILD"
  echo "              exe $FIRST_EXE map $FIRST_MAP disc $FIRST_DISC"
  echo "seed build    ${SEED_BUILD:-(none: the committed layout must bind)}"
  echo "ship build    $SHIP_BUILD"
  echo "              exe $SHIP_EXE map $SHIP_MAP disc $SHIP_DISC"
  echo "search        rounds $ROUNDS seed $SEED batch $BATCH threads $THREADS start $START (no early stop)"
  echo "launch args   $LAUNCH_ARGS"
  echo "windows:"
  echo "$WINDOWS" | while read -r n t a b w extra; do
    [ -n "$n" ] || continue
    printf '  %-16s tape %-40s polls %s..%s weight %s %s\n' "$n" "$t" "$a" "$b" "$w" "$extra"
    [ "$t" = - ] || [ -f "$repo/$t" ] || echo "  ^ MISSING tape $repo/$t"
  done
  exit 0
fi

say "game $GAME, repo $repo at $(git -C "$repo" rev-parse --short=12 HEAD), work $work"

# ---- 1. first: the unordered link of the shipping code, and a disc of it to trace ----------
if runs first; then
  step "first link"
  build_into first "$FIRST_BUILD" "$FIRST_EXE" "$FIRST_MAP" "$FIRST_DISC" "$work/first"
fi
[ -f "$work/first/image.exe" ] && [ -f "$work/first/link.map" ] || die "no first link in $work/first (run from 'first')"
# Checked after the first link: a game whose build writes out the script it links with has it
# only from then on.
[ -f "$script_path" ] || die "no linker script at $script_path (LINKER_SCRIPT must be the one the ship link uses)"

# ---- 2. seed: a layout that binds to the first link, to start the search from -------------
if runs seed; then
  step "seed layout"
  binds=0
  if [ "$reseed" = 0 ] && [ -f "$layout_path" ]; then
    if pgo place "$layout_path" "$work/first/link.map" "$work/first/image.exe" "$script_path" \
         "$work/seed-check.order" > "$work/seed-check.log" 2>&1; then binds=1; fi
    cat "$work/seed-check.log"
  fi
  if [ "$binds" = 0 ]; then
    [ -n "$SEED_BUILD" ] || die "the committed layout does not bind to the first link and the config has no SEED_BUILD"
    say "seed build: $SEED_BUILD"
    (cd "$repo" && eval "$SEED_BUILD") > "$work/seed.log" 2>&1 || { tail -40 "$work/seed.log" >&2; die "seed build failed (log $work/seed.log)"; }
    pgo place "$layout_path" "$work/first/link.map" "$work/first/image.exe" "$script_path" \
      "$work/seed-check.order" || die "the seed layout does not bind to the first link either"
  fi
  cp -c "$layout_path" "$work/seed.layout"
fi
[ -f "$work/seed.layout" ] || die "no seed layout in $work (run from 'seed')"

# ---- 3. trace: one fetch trace per window, on the first-link disc ---------------------------
# The trace binds by portable name and code hash, so the unordered disc of the same code serves
# (psoxide-pgo README, "search"). Each window: a short replay to find the route tick of its
# first poll, then the traced replay up to its last poll.
if runs trace; then
  step "fetch traces"
  rm -rf "$work/trace"; mkdir -p "$work/trace"
  while read -r name tape from_poll to_poll weight extra; do
    [ -n "$name" ] || continue
    d=$work/trace/$name; mkdir -p "$d/cfg"
    input=(); [ "$tape" = - ] || input=(--input-tape "$repo/$tape")
    # shellcheck disable=SC2086
    emu --path "$work/first/disc.cue" --config-dir "$d/cfg" ${input[@]+"${input[@]}"} $LAUNCH_ARGS $extra \
      --steps "$STEPS" --stop-at-poll $((from_poll + 30)) --route-log "$d/locate.csv" > "$d/locate.log" 2>&1 \
      || die "$name: locate replay failed (log $d/locate.log)"
    start=$(awk -F, -v p="$from_poll" 'NR==1{for(i=1;i<=NF;i++){if($i=="route_tick")a=i;if($i=="port1_polls")b=i};next} a&&b&&$b>=p{print $a; exit}' "$d/locate.csv")
    [ -n "$start" ] || die "$name: the replay never reached poll $from_poll (log $d/locate.log)"
    # shellcheck disable=SC2086
    emu --path "$work/first/disc.cue" --config-dir "$d/cfg" ${input[@]+"${input[@]}"} $LAUNCH_ARGS $extra \
      --steps "$STEPS" --stop-at-poll "$to_poll" --fetch-trace-log "$work/trace/$name.trace" \
      --fetch-trace-start-route-tick "$start" > "$d/trace.log" 2>&1 \
      || die "$name: traced replay failed (log $d/trace.log)"
    [ -s "$work/trace/$name.trace" ] || die "$name: empty trace"
    say "$name: polls $from_poll..$to_poll from route tick $start, trace $(du -h "$work/trace/$name.trace" | cut -f1)"
  done <<EOF
$WINDOWS
EOF
fi

# ---- 4. search: every round, from the seed's greedy placement --------------------------------
if runs search; then
  step "search"
  args=()
  while read -r name tape from_poll to_poll weight extra; do
    [ -n "$name" ] || continue
    [ -s "$work/trace/$name.trace" ] || die "no trace for window $name (run from 'trace')"
    args+=(--trace "$name" "$work/first/link.map" "$work/first/image.exe" "$work/trace/$name.trace")
    [ "${weight:-1}" = 1 ] || args+=(--weight "$name=$weight")
  done <<EOF
$WINDOWS
EOF
  [ ${#args[@]} -gt 0 ] || die "WINDOWS lists no window"
  if ! pgo search "$work/seed.layout" "$work/first/link.map" "$work/first/image.exe" "$script_path" \
      "$layout_path" "${args[@]}" --rounds "$ROUNDS" --seed "$SEED" --batch "$BATCH" \
      --threads "$THREADS" --start "$START" 2>&1 | tee "$work/search.log"; then
    die "psoxide-pgo search failed (log $work/search.log)"
  fi
  grep -q "^search: seed $SEED rounds $ROUNDS batch $BATCH: " "$work/search.log" \
    || die "the search log does not show $ROUNDS rounds at seed $SEED"
fi
stamp=$(awk '/^#/{ if (index($0, "# psoxide-pgo searched: ") == 1) { print; exit } next } { exit }' "$layout_path")
[ "$stamp" = "# psoxide-pgo searched: rounds $ROUNDS seed $SEED" ] \
  || die "$LAYOUT carries '${stamp:-no search stamp}', not rounds $ROUNDS seed $SEED"
say "stamp: $stamp"

# ---- 5. ship: the game's shipping build, now linking the searched layout ---------------------
if runs ship; then
  step "ship build"
  build_into ship "$SHIP_BUILD" "$SHIP_EXE" "$SHIP_MAP" "$SHIP_DISC" "$work/ship"
fi
[ -f "$work/ship/link.map" ] || die "no ship link in $work/ship (run from 'ship')"

# ---- 6. check: the ship map follows the searched placement ----------------------------------
# The order the searched layout gives the first link, written independently of the game's build
# (its build may place silently, fall back, or never check the relink), then checked against the
# ship map. ld.lld drops a name it cannot find without a word, so this is the proof.
step "check order"
rm -rf "$work/check"; mkdir -p "$work/check"
if ! pgo place "$layout_path" "$work/first/link.map" "$work/first/image.exe" "$script_path" \
    "$work/check/order.txt" 2>&1 | tee "$work/check/place.log"; then
  die "the searched layout does not place on the first link (log $work/check/place.log)"
fi
grep -q "^recorded placement: " "$work/check/place.log" \
  || die "place did not use the layout's recorded placement (is $LAYOUT the searched one?)"
if ! pgo followed "$work/check/order.txt" "$work/ship/link.map" 2>&1 | tee "$work/check/followed.log"; then
  die "the ship map does not follow the searched layout (log $work/check/followed.log)"
fi
line=$(grep "^followed: " "$work/check/followed.log") || die "no 'followed:' line from psoxide-pgo"
at=$(echo "$line" | sed -n 's/.*; \([0-9]*\) of the \([0-9]*\) placed.*/\1/p')
of=$(echo "$line" | sed -n 's/.*; \([0-9]*\) of the \([0-9]*\) placed.*/\2/p')
[ -n "$at" ] && [ -n "$of" ] || die "cannot read '$line'"
drift=$((of - at))
[ "$drift" -le "$MAX_DRIFT" ] \
  || die "$drift of $of placed functions are not where the search scored them (MAX_DRIFT=$MAX_DRIFT): the ship link does not match the first link or the linker script"
if cmp -s "$work/first/link.map" "$work/ship/link.map"; then die "the ship map equals the unordered first map"; fi

step "done"
say "PASS: $LAYOUT searched ($stamp), ship map follows it, $at of $of placed functions at the searched address"
say "ship disc $work/ship/disc.cue, exe $(shasum -a 256 "$work/ship/image.exe" | cut -c1-12)"
cat <<EOF

Next (this script commits and blesses nothing):
  1. git -C $repo add $LAYOUT && git -C $repo commit -m "pgo: searched layout ($ROUNDS rounds, seed $SEED)"
  2. psoxide-test run --tier quick --repo $repo      speed against the current baseline
  3. psoxide-test bless --repo $repo                 the searched layout's baseline
  4. psoxide-test run --tier full --repo $repo       then land, then install (install profile)
  The traces in $work/trace are working files: delete them once the layout is committed.
EOF
