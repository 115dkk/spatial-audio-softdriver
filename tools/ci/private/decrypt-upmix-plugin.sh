#!/usr/bin/env bash
# Decrypts the third-party test upmixer into the runner's temp directory for
# the plugin host job (tools/ci/private/README.md). Never into the checkout,
# never echoed, never uploaded. Writes `path=<file>` to $GITHUB_OUTPUT, or
# `path=` when the key is absent (pull requests from forks get no secrets),
# so the caller skips the third-party plugin tests.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
expected=9ddcd1a9a71a4f0228754f6e6eb5b08715330fa80cfc11dd0570796c806555bd
if [ -z "${UPMIX_PLUGIN_KEY:-}" ]; then
  echo "UPMIX_PLUGIN_KEY is not set: skipping the third-party upmixer"
  echo "path=" >> "${GITHUB_OUTPUT:-/dev/null}"
  exit 0
fi
out_dir="${RUNNER_TEMP:?}/upmix-plugin"
mkdir -p "$out_dir"
out="$out_dir/OpenSpatial_Upmixer.vst3"
gpg --batch --yes --quiet --pinentry-mode loopback --passphrase-fd 0 \
  -o "$out" -d "$here/openspatial-upmixer.vst3.gpg" <<< "$UPMIX_PLUGIN_KEY"
actual="$(sha256sum "$out" | cut -d' ' -f1)"
if [ "$actual" != "$expected" ]; then
  rm -f "$out"
  echo "the decrypted upmixer does not match its SHA-256" >&2
  exit 1
fi
echo "path=$out" >> "${GITHUB_OUTPUT:-/dev/null}"
