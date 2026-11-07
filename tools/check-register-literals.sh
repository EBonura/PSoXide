#!/bin/sh
# Register addresses live in crates/psx-hw and nowhere else. Fails when a
# hardware-window address literal (physical 0x1F80_xxxx, its uncached view
# 0xBF80_xxxx, or psx-spx's 1F80xxxxh spelling) appears in any other tracked
# text file. Name the register through psx_hw instead, in code and in docs.
# Moved in from PSoXide-editor and not yet converted: the hardware-test suite reads the registers
# by raw address on purpose (independent of psx_hw), its reference captures and docs quote them, and
# psx-engine, psx-goldsrc and psx-chainloader still carry a few literals (follow-up: use psx_hw).
# The generated website data and vendored font files are not ours to edit.
set -eu
cd "$(dirname "$0")/.."
if hits=$(git grep -nIiE '0x1f80|0xbf80|1f80_|1f80[0-9a-f]{4}h' -- . \
	':!crates/psx-hw' ':!engine/examples/hardware-tests' ':!docs/hardware-refs' ':!engine/crates/psx-engine' ':!engine/crates/psx-goldsrc' ':!engine/crates/psx-chainloader' ':!docs/engine-harmonisation-plan-2026-08-22.md' ':!docs/shared-engine-standardisation-2026-08-22.md' ':!engine/examples/editor-playtest/src/playtest_scene.rs' ':!docs/hardware-test-*' ':!tools/psoxide-hwtest' ':!website/data' ':!sdk/crates/psx-font/vendor' ':!tools/check-register-literals.sh'); then
	echo "register address literals outside crates/psx-hw (use psx_hw):" >&2
	echo "$hits" >&2
	exit 1
fi
