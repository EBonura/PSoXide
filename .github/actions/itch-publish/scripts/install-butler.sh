#!/usr/bin/env bash
set -euo pipefail

: "${RUNNER_TEMP:?RUNNER_TEMP is required}"

install_dir="${RUNNER_TEMP}/butler"
archive="${RUNNER_TEMP}/butler.zip"
mkdir -p "${install_dir}"
curl --fail --location --retry 4 --retry-all-errors \
  --output "${archive}" \
  https://broth.itch.zone/butler/linux-amd64/LATEST/archive/default
unzip -q -o "${archive}" -d "${install_dir}"
chmod 0755 "${install_dir}/butler"
"${install_dir}/butler" -V
