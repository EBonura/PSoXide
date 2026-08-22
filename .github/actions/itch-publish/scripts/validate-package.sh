#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'itch package validation failed: %s\n' "$*" >&2
  exit 1
}

: "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${ITCH_SOURCE_DIR:?ITCH_SOURCE_DIR is required}"
: "${ITCH_TARGET:?ITCH_TARGET is required}"
: "${ITCH_USER_VERSION:?ITCH_USER_VERSION is required}"

case "${GITHUB_REPOSITORY}:${ITCH_TARGET}" in
  EBonura/voxide:bonnie-studios/voxide:psx | \
  EBonura/celeste-collection-psx:bonnie-studios/celeste-classic-collection-psx:psx | \
  EBonura/nitroxide:bonnie-studios/nitroxide:psx | \
  EBonura/PSoXide:bonnie-studios/psoxide:html5)
    ;;
  *)
    fail "${GITHUB_REPOSITORY} is not allowed to publish ${ITCH_TARGET}"
    ;;
esac

if [[ ! "${ITCH_USER_VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z][0-9A-Za-z.-]*)?$ ]]; then
  fail "user version must be SemVer-compatible: ${ITCH_USER_VERSION}"
fi

[[ ! -L "${ITCH_SOURCE_DIR}" ]] || fail "source directory must not be a symlink"
[[ -d "${ITCH_SOURCE_DIR}" ]] || fail "source directory does not exist: ${ITCH_SOURCE_DIR}"

workspace="$(cd "${GITHUB_WORKSPACE}" && pwd -P)"
source_dir="$(cd "${ITCH_SOURCE_DIR}" && pwd -P)"
case "${source_dir}" in
  "${workspace}"/*) ;;
  *) fail "source directory must stay inside GITHUB_WORKSPACE" ;;
esac

if [[ -n "$(find "${source_dir}" -type l -print -quit)" ]]; then
  fail "package contains a symlink"
fi
if [[ -z "$(find "${source_dir}" -type f -print -quit)" ]]; then
  fail "package contains no files"
fi

file_count="$(find "${source_dir}" -type f | wc -l | tr -d ' ')"
byte_count=0
while IFS= read -r -d '' package_file; do
  package_file_bytes="$(wc -c < "${package_file}" | tr -d ' ')"
  byte_count="$((byte_count + package_file_bytes))"
done < <(find "${source_dir}" -type f -print0)
printf 'itch package verified: repository=%s target=%s version=%s files=%s bytes=%s\n' \
  "${GITHUB_REPOSITORY}" "${ITCH_TARGET}" "${ITCH_USER_VERSION}" \
  "${file_count}" "${byte_count}"
