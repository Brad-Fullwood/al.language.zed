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

# `sed -i` is not portable: GNU takes an optional suffix, BSD/macOS *requires*
# one, so a bare `-i` fails on macOS (a supported dev platform — CI builds on
# macos-latest). Build a mode-preserving replacement beside the target, then
# rename it atomically over the original.
sed_inplace() { # $1 = sed expression, $2 = file
  local expression="$1"
  local target="$2"
  local target_dir target_name tmp

  if [[ ! -f "$target" || -L "$target" ]]; then
    echo "ERROR: atomic rewrite target must be a regular non-symlink file: $target" >&2
    return 1
  fi

  target_dir="$(dirname "$target")" || return 1
  target_name="$(basename "$target")" || return 1
  tmp="$(mktemp "$target_dir/.${target_name}.tmp.XXXXXX")" || return 1

  # POSIX `cp -p` transfers the existing mode to the already-created temporary
  # file without GNU-only `chmod --reference`. The temp lives in the target
  # directory, so the final rename cannot cross filesystems.
  if ! cp -p "$target" "$tmp"; then
    rm -f "$tmp"
    echo "ERROR: failed to preserve metadata for $target" >&2
    return 1
  fi

  # A failing or interrupted sed can damage only the temporary file. The
  # original remains byte-identical until the single same-directory rename.
  if ! sed "$expression" "$target" > "$tmp"; then
    rm -f "$tmp"
    echo "ERROR: failed to rewrite $target (sed expression: $expression)" >&2
    return 1
  fi
  if ! mv -f "$tmp" "$target"; then
    rm -f "$tmp"
    echo "ERROR: failed to install atomic rewrite for $target" >&2
    return 1
  fi
}

# Allow the atomic helper to be sourced by its cross-platform contract test
# without switching dependencies or rebuilding the extension.
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
  return 0
fi

cd "$(dirname "$0")/.."

DEV_VER="0.8.0"
STABLE_VER="0.7.0"
DEV_DEP='zed_extension_api = { git = "https://github.com/zed-industries/zed", branch = "main" }'
STABLE_DEP="zed_extension_api = \"$STABLE_VER\""

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
cargo build -p zed-al --target wasm32-wasip2 --release
bash scripts/check-zed-wasm-component.sh target/wasm32-wasip2/release/zed_al.wasm
echo "Done. Reload the extension in Zed to apply."
