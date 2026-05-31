#!/usr/bin/env bash
# Switch which Zed extension API the AL extension targets.
#
#   dev     unreleased API (git main, v0.8.0). Loads ONLY on Zed dev/nightly/
#           preview builds. Use on machines running bleeding-edge Zed.
#   stable  latest RELEASED API (v0.6.0). Loads on stable Zed and is required
#           for the public extension registry. A released API also works on
#           dev Zed, so "stable" is the safe default for shipping.
#
# Usage:
#   scripts/use-api.sh stable   # public / stable-Zed machines
#   scripts/use-api.sh dev      # this machine if it runs dev/nightly Zed
#   scripts/use-api.sh show     # print current target
#
# After switching, the WASM is rebuilt. In Zed, reload the dev extension
# (command palette: "zed: reload extensions" or reinstall dev extension).
set -euo pipefail
cd "$(dirname "$0")/.."

DEV_VER="0.8.0"
STABLE_VER="0.6.0"
DEV_DEP='zed_extension_api = { git = "https://github.com/zed-industries/zed", branch = "main" }'
STABLE_DEP="zed_extension_api = \"$STABLE_VER\""

set_lib_version() { # $1 = version
  # Replace `version = "..."` only inside the [lib] table of extension.toml.
  sed -i "/^\[lib\]/,/^\[/ s/^version = .*/version = \"$1\"/" extension.toml
}

case "${1:-}" in
  dev)
    sed -i "s#^zed_extension_api = .*#${DEV_DEP//#/\\#}#" Cargo.toml
    set_lib_version "$DEV_VER"
    echo "→ DEV API (git main, $DEV_VER). Loads on Zed dev/nightly only." ;;
  stable)
    sed -i "s#^zed_extension_api = .*#${STABLE_DEP}#" Cargo.toml
    set_lib_version "$STABLE_VER"
    echo "→ STABLE API ($STABLE_VER). Loads on stable Zed + public registry." ;;
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
