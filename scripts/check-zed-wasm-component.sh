#!/usr/bin/env bash
# Zed instantiates Rust extensions as WebAssembly components. A successful
# wasm32-wasip1 build is only a core module and cannot be loaded by Zed.
set -euo pipefail

artifact="${1:-}"
if [ -z "${artifact}" ] || [ ! -f "${artifact}" ]; then
    echo "ERROR: expected a Zed extension artifact path" >&2
    exit 2
fi

# Component binaries begin with the WebAssembly magic followed by component
# encoding version 0x0001000d. Core modules instead use 0x00000001.
header="$(od -An -tx1 -N8 "${artifact}" | tr -d ' \n')"
if [ "${header}" != "0061736d0d000100" ]; then
    echo "ERROR: ${artifact} is not a WebAssembly component (header ${header:-missing})." >&2
    echo "Build zed-al for wasm32-wasip2; wasm32-wasip1 output cannot load in Zed." >&2
    exit 1
fi

echo "OK: ${artifact} is a WebAssembly component"
