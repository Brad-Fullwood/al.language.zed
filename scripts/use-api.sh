#!/usr/bin/env bash
# Switch which Zed extension API the AL extension targets.
#
#   dev     unreleased API (git main, v0.8.0). Loads ONLY on Zed dev/nightly
#           builds. LOCAL EXPERIMENTS ONLY -- never commit this state: the
#           committed_api_target_is_released guard test fails on it. In this
#           mode build.rs detects 0.8 in Cargo.lock and enables the
#           `zed_api_0_8` cfg, so src/lib.rs's settings-schema methods
#           (settings.json autocomplete for the AL settings block) compile in
#           automatically -- no source edit needed.
#   stable  latest RELEASED API (v0.7.0, full DAP/locator support). Loads on
#           ALL Zed channels and is required for the public extension
#           registry. This is the committed/shipping state.
#
# Usage:
#   scripts/use-api.sh stable   # the default, committed state
#   scripts/use-api.sh dev      # local experiments against zed git main
#   scripts/use-api.sh show     # print current target
#
# After switching, the WASM is rebuilt. In Zed, reload the dev extension
# (command palette: "zed: reload extensions" or reinstall dev extension).
set -euo pipefail
cd "$(dirname "$0")/.."

DEV_VER="0.8.0"
STABLE_VER="0.7.0"
DEV_DEP='zed_extension_api = { git = "https://github.com/zed-industries/zed", branch = "main" }'
STABLE_DEP="zed_extension_api = \"$STABLE_VER\""

# `sed -i` is not portable: GNU takes an optional suffix, BSD/macOS *requires*
# one, so a bare `-i` fails on macOS (a supported dev platform — CI builds on
# macos-latest). Edit through a temp file instead, which behaves the same
# everywhere.
sed_inplace() { # $1 = sed expression, $2 = file
  local tmp
  tmp="$(mktemp)"
  sed "$1" "$2" > "$tmp"
  cat "$tmp" > "$2"
  rm -f "$tmp"
}

set_lib_version() { # $1 = version
  # Replace `version = "..."` only inside the [lib] table of extension.toml.
  sed_inplace "/^\[lib\]/,/^\[/ s/^version = .*/version = \"$1\"/" extension.toml
}

case "${1:-}" in
  dev)
    sed_inplace "s#^zed_extension_api = .*#${DEV_DEP//#/\\#}#" Cargo.toml
    set_lib_version "$DEV_VER"
    echo "DEV API (git main, $DEV_VER). Loads on Zed dev/nightly only." ;;
  stable)
    sed_inplace "s#^zed_extension_api = .*#${STABLE_DEP}#" Cargo.toml
    set_lib_version "$STABLE_VER"
    echo "STABLE API ($STABLE_VER). Loads on stable Zed + public registry." ;;
  show)
    echo "Cargo.toml : $(grep -m1 '^zed_extension_api' Cargo.toml)"
    echo "extension  : [lib] version = $(awk '/^\[lib\]/{f=1} f&&/^version/{print $3; exit}' extension.toml)"
    exit 0 ;;
  *)
    echo "Usage: $0 dev|stable|show" >&2; exit 1 ;;
esac

echo "Rebuilding WASM extension..."
cargo build -p zed-al --target wasm32-wasip1 --release
echo "Done. Reload the extension in Zed to apply."
