#!/usr/bin/env bash
# check-glibc-floor.sh: fail when an ELF binary needs a glibc newer than a floor.
#
# Usage:
#   ./scripts/check-glibc-floor.sh <floor> <binary>...
#
# <floor> is a dotted version such as 2.31. Each binary is read with
# `readelf -V --wide`, which lists the GLIBC_ symbol versions it needs. The
# highest one is compared with the floor and printed.
#
# Exit 0 when every binary stays at or below the floor, 1 when one needs a
# newer version, 2 on bad arguments or a file that readelf cannot read.

set -euo pipefail

fail() {
    echo "ERROR: $*" >&2
    exit 2
}

if [ "$#" -lt 2 ]; then
    fail "usage: $0 <floor> <binary>..."
fi

floor="$1"
shift

case "${floor}" in
    ""|*[!0-9.]*) fail "floor must be a dotted version such as 2.31, got: ${floor}" ;;
esac

status=0
for binary in "$@"; do
    [ -f "${binary}" ] || fail "no such file: ${binary}"
    symbols="$(readelf -V --wide "${binary}" 2>/dev/null)" \
        || fail "readelf cannot read ${binary} (not an ELF file?)"
    # grep exits 1 when the binary names no GLIBC_ version, which is not an error here.
    versions="$(printf '%s\n' "${symbols}" | grep -o 'GLIBC_[0-9][0-9.]*' || true)"
    highest="$(printf '%s\n' "${versions}" | sed 's/^GLIBC_//' | sort -V | tail -n1)"
    if [ -z "${highest}" ]; then
        echo "OK: ${binary}: no GLIBC_ version, floor ${floor}"
        continue
    fi
    newest="$(printf '%s\n' "${floor}" "${highest}" | sort -V | tail -n1)"
    if [ "${newest}" != "${floor}" ]; then
        echo "ERROR: ${binary} needs GLIBC_${highest}, above the floor GLIBC_${floor}" >&2
        status=1
    else
        echo "OK: ${binary}: highest GLIBC_${highest}, floor ${floor}"
    fi
done

exit "${status}"
