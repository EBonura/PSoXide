#!/usr/bin/env bash
set -euo pipefail

: "${BUTLER_API_KEY:?BUTLER_API_KEY repository secret is required for publishing}"
: "${BUTLER_BIN:?BUTLER_BIN is required}"
: "${ITCH_SOURCE_DIR:?ITCH_SOURCE_DIR is required}"
: "${ITCH_TARGET:?ITCH_TARGET is required}"
: "${ITCH_USER_VERSION:?ITCH_USER_VERSION is required}"

[[ -x "${BUTLER_BIN}" ]] || {
  printf 'butler executable is unavailable: %s\n' "${BUTLER_BIN}" >&2
  exit 1
}

game_target="${ITCH_TARGET%:*}"
"${BUTLER_BIN}" status "${game_target}"
"${BUTLER_BIN}" push "${ITCH_SOURCE_DIR}" "${ITCH_TARGET}" \
  --userversion "${ITCH_USER_VERSION}"
