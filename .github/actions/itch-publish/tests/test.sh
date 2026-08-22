#!/usr/bin/env bash
set -euo pipefail

action_dir="$(cd "$(dirname "$0")/.." && pwd -P)"
validate="${action_dir}/scripts/validate-package.sh"
publish="${action_dir}/scripts/publish.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/itch-publish-test.XXXXXX")"
trap 'rm -rf "${scratch}"' EXIT

workspace="${scratch}/workspace"
package="${workspace}/package"
mkdir -p "${package}"
printf 'disc image\n' > "${package}/game.bin"

run_validate() {
  GITHUB_WORKSPACE="${workspace}" \
  GITHUB_REPOSITORY="${1}" \
  ITCH_SOURCE_DIR="${2}" \
  ITCH_TARGET="${3}" \
  ITCH_USER_VERSION="${4}" \
    "${validate}"
}

expect_failure() {
  if "$@" >/dev/null 2>&1; then
    printf 'expected failure: %s\n' "$*" >&2
    exit 1
  fi
}

run_validate EBonura/voxide "${package}" bonnie-studios/voxide:psx 1.2.3
run_validate EBonura/PSoXide "${package}" bonnie-studios/psoxide:html5 0.20.0-dev.abc123
expect_failure run_validate EBonura/voxide "${package}" bonnie-studios/nitroxide:psx 1.2.3
expect_failure run_validate EBonura/voxide "${package}" bonnie-studios/voxide:psx v1.2.3

empty="${workspace}/empty"
mkdir -p "${empty}"
expect_failure run_validate EBonura/voxide "${empty}" bonnie-studios/voxide:psx 1.2.3

outside="${scratch}/outside"
mkdir -p "${outside}"
printf 'outside\n' > "${outside}/file"
expect_failure run_validate EBonura/voxide "${outside}" bonnie-studios/voxide:psx 1.2.3

ln -s "${package}/game.bin" "${package}/linked.bin"
expect_failure run_validate EBonura/voxide "${package}" bonnie-studios/voxide:psx 1.2.3
rm "${package}/linked.bin"

fake_butler="${scratch}/butler"
butler_log="${scratch}/butler.log"
cat > "${fake_butler}" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${BUTLER_TEST_LOG}"
EOF
chmod +x "${fake_butler}"

expect_failure env -u BUTLER_API_KEY \
  BUTLER_BIN="${fake_butler}" ITCH_SOURCE_DIR="${package}" \
  ITCH_TARGET=bonnie-studios/voxide:psx ITCH_USER_VERSION=1.2.3 \
  "${publish}"

BUTLER_API_KEY=test-only BUTLER_BIN="${fake_butler}" \
BUTLER_TEST_LOG="${butler_log}" ITCH_SOURCE_DIR="${package}" \
ITCH_TARGET=bonnie-studios/voxide:psx ITCH_USER_VERSION=1.2.3 \
  "${publish}"

grep -Fxq 'status bonnie-studios/voxide' "${butler_log}"
grep -Fxq "push ${package} bonnie-studios/voxide:psx --userversion 1.2.3" "${butler_log}"
printf 'itch publisher tests: PASS\n'
