#!/bin/sh
set -eu

BASE_URL="${SPACE_CHANNEL_BASE_URL:-https://deadeyenas.tailcc82b.ts.net/space-channel}"
ROOT="${SPACE_CHANNEL_INSTALL_ROOT:-${HOME}/.local/share/space-channel}"
PLUGIN_DIR="${ROOT}/plugin"
BIN_DIR="${PLUGIN_DIR}/bin"

uname_s="$(uname -s)"
uname_m="$(uname -m)"
case "${uname_s}/${uname_m}" in
  Darwin/arm64)        ASSET="space-channel-darwin-arm64" ;;
  Linux/x86_64)        ASSET="space-channel-linux-x64" ;;
  Linux/amd64)         ASSET="space-channel-linux-x64" ;;
  *) echo "unsupported platform: ${uname_s}/${uname_m}" >&2; exit 1 ;;
esac

fetch() {
  curl -fsSL --connect-timeout 5 -o "$2" "${BASE_URL}/$1"
}

mkdir -p "${BIN_DIR}"
STAGE="$(mktemp -d "${ROOT}/.stage.XXXXXX")"
trap 'rm -rf "${STAGE}"' EXIT

echo "fetching ${ASSET}..."
fetch "${ASSET}" "${STAGE}/space-channel"
chmod +x "${STAGE}/space-channel"

echo "fetching plugin.tar.gz..."
fetch "plugin.tar.gz" "${STAGE}/plugin.tar.gz"
mkdir -p "${STAGE}/plugin"
tar -xzf "${STAGE}/plugin.tar.gz" -C "${STAGE}/plugin"

mv -f "${STAGE}/space-channel" "${BIN_DIR}/space-channel"

for rel in $(cd "${STAGE}/plugin" && find . -type f | sed 's|^\./||'); do
  mkdir -p "${PLUGIN_DIR}/$(dirname "${rel}")"
  cp "${STAGE}/plugin/${rel}" "${PLUGIN_DIR}/${rel}.tmp"
  mv -f "${PLUGIN_DIR}/${rel}.tmp" "${PLUGIN_DIR}/${rel}"
done

rm -f "${ROOT}/space-channel"
mkdir -p "${HOME}/.local/bin"
ln -sf "${BIN_DIR}/space-channel" "${HOME}/.local/bin/space-channel"

echo "installed ${PLUGIN_DIR} (binary $("${BIN_DIR}/space-channel" version), plugin $(cat "${PLUGIN_DIR}/VERSION" 2>/dev/null || echo unknown))"
