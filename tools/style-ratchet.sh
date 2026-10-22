#!/bin/sh
# Style ratchet for the lints of Google's Rust style guide that are not yet
# clean. Each is `allow` in the workspace manifests (so `-D warnings` stays
# green) and measured here with `-W`. A count may go down, never up:
#
#   tools/style-ratchet.sh            fail when any count exceeds the baseline
#   tools/style-ratchet.sh --update   rewrite the baseline to today's counts
#   tools/style-ratchet.sh --report   print the counts and the top files, never fail
#
# Counted: library and binary targets only (tests may unwrap and panic, as the
# guide allows), host workspaces plus the SDK on the PS1 target, because the
# guest-only code is compiled out of a host build. Needs jq.
set -eu
cd "$(dirname "$0")/.."

BASELINE=tools/style-ratchet.baseline
MODE=${1:-check}

LINTS="clippy::must_use_candidate clippy::unwrap_used clippy::exhaustive_enums
clippy::exhaustive_structs clippy::missing_errors_doc clippy::missing_panics_doc
clippy::wildcard_imports clippy::string_add clippy::string_add_assign
clippy::fn_params_excessive_bools clippy::needless_for_each
clippy::disallowed_types clippy::disallowed_methods missing_docs"

WARN=""
for l in $LINTS; do WARN="$WARN -W $l"; done

# measure CONTEXT CARGO_ARGS...  prints "CONTEXT LINT COUNT" for every ratchet lint
measure() {
	ctx=$1
	shift
	out=$(cargo clippy --locked "$@" --message-format=json -- $WARN 2>/dev/null |
		jq -r 'select(.reason == "compiler-message") | .message.code.code // empty' || true)
	for l in $LINTS; do
		n=$(printf '%s\n' "$out" | grep -c -x -F "$l" || true)
		echo "$ctx $l $n"
	done
}

NOW=$(mktemp)
CONF=$(mktemp -d)
trap 'rm -rf "$NOW" "$CONF"' EXIT
# clippy.toml plus the Rc/Arc and catch_unwind lists, for this run only
cat clippy.toml tools/style-ratchet-clippy.toml >"$CONF/clippy.toml"
export CLIPPY_CONF_DIR="$CONF"
{
	measure root --workspace --lib --bins --all-features
	measure sdk --manifest-path sdk/Cargo.toml --workspace --lib --bins --all-features
	measure sdk-guest --manifest-path sdk/Cargo.toml --workspace --lib --target mipsel-sony-psx \
		-Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
} >"$NOW"

case $MODE in
--update)
	cp "$NOW" "$BASELINE"
	echo "style-ratchet: baseline rewritten ($BASELINE)"
	exit 0
	;;
--report)
	cat "$NOW"
	exit 0
	;;
esac

fail=0
while read -r ctx lint n; do
	base=$(awk -v c="$ctx" -v l="$lint" '$1 == c && $2 == l { print $3 }' "$BASELINE")
	base=${base:-0}
	if [ "$n" -gt "$base" ]; then
		echo "style-ratchet: $ctx $lint rose from $base to $n" >&2
		fail=1
	elif [ "$n" -lt "$base" ]; then
		echo "style-ratchet: $ctx $lint fell from $base to $n; run tools/style-ratchet.sh --update"
	fi
done <"$NOW"
exit $fail
