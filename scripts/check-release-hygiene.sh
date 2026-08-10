#!/usr/bin/env bash
# check-release-hygiene.sh -- Release-blocking invariants.
#
# This is intentionally broader than scripts/check-repo-consistency.sh. That
# script only guards the GitHub repository slug used for binary downloads; this
# script guards the release state itself: version alignment, tag/version match,
# grammar submodule/rev alignment, generated-asset co-change, and optionally
# green CI on the exact commit being released.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

TAG_NAME=""
CHANGED_SINCE=""
REQUIRE_CI=0
CI_TIMEOUT_SECONDS="${CI_TIMEOUT_SECONDS:-3600}"
CI_POLL_SECONDS="${CI_POLL_SECONDS:-30}"
CI_WORKFLOW="${CI_WORKFLOW:-CI}"
# ci.yml only triggers on push to main/dev; a tag pushed on a commit that
# never landed on one of those branches (or tagged before the branch push
# propagated) will NEVER get a CI run — waiting the full CI_TIMEOUT_SECONDS
# for one is just a slow, unhelpful failure. Bound how long we wait to see
# ANY run appear at all (queued/in_progress/etc.) before giving up with an
# actionable message; once a run is discovered, the full CI_TIMEOUT_SECONDS
# still applies to waiting for it to finish.
CI_DISCOVERY_TIMEOUT_SECONDS="${CI_DISCOVERY_TIMEOUT_SECONDS:-120}"
REGENERATE=0
FULL_REGENERATE=0

usage() {
    cat <<'USAGE'
Usage:
  scripts/check-release-hygiene.sh [options]

Options:
  --tag <tag>                 Require tag to equal v<package-version> and point at HEAD.
  --changed-since <ref|auto>  Check generated-source/output co-change since ref.
                              "auto" uses the previous v* tag before --tag.
  --require-ci                Wait for a successful CI workflow run on HEAD.
  --ci-timeout-seconds <n>    Timeout for --require-ci (default: 3600).
  --regenerate                Run make grammar and fail if generated outputs change.
  --full-regenerate           Requires a pinned unpacked Microsoft AL extension
                              at AL_EXTENSION_PATH; regenerates the complete
                              grammar/data/theme output, verifies the Record
                              method catalog, and runs the pinned corpus.
  -h, --help                  Show this help.

Environment:
  GITHUB_REPOSITORY            owner/repo for --require-ci in GitHub Actions.
  GH_TOKEN or GITHUB_TOKEN     token used by gh for --require-ci.
  CI_WORKFLOW                  workflow name to require (default: CI).
  CI_DISCOVERY_TIMEOUT_SECONDS how long --require-ci waits for a FIRST run to
                                appear for the tagged commit before failing
                                fast, instead of hanging for the full
                                CI_TIMEOUT_SECONDS (default: 120). ci.yml only
                                runs on push to main/dev, so a commit that
                                never reached one of those branches will never
                                get a run at all.
USAGE
}

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

ok() {
    echo "OK: $*"
}

warn() {
    echo "WARNING: $*" >&2
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --tag)
            TAG_NAME="${2:-}"
            [ -n "${TAG_NAME}" ] || fail "--tag requires a value"
            shift 2
            ;;
        --changed-since)
            CHANGED_SINCE="${2:-}"
            [ -n "${CHANGED_SINCE}" ] || fail "--changed-since requires a value"
            shift 2
            ;;
        --require-ci)
            REQUIRE_CI=1
            shift
            ;;
        --ci-timeout-seconds)
            CI_TIMEOUT_SECONDS="${2:-}"
            [ -n "${CI_TIMEOUT_SECONDS}" ] || fail "--ci-timeout-seconds requires a value"
            shift 2
            ;;
        --regenerate)
            REGENERATE=1
            shift
            ;;
        --full-regenerate)
            FULL_REGENERATE=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
done

cd "${REPO_ROOT}"

toml_first_string() {
    local key="$1"
    local file="$2"
    awk -F'"' -v key="${key}" '
        $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
            print $2
            exit
        }
    ' "${file}"
}

package_name() {
    toml_first_string "name" "$1"
}

package_version() {
    toml_first_string "version" "$1"
}

lock_version_for() {
    local name="$1"
    awk -F'"' -v wanted="${name}" '
        $0 == "[[package]]" {
            if (pkg == wanted && version != "") {
                print version
                found = 1
                exit
            }
            pkg = ""
            version = ""
            in_pkg = 1
            next
        }
        in_pkg && /^[[:space:]]*name[[:space:]]*=/ {
            pkg = $2
            next
        }
        in_pkg && /^[[:space:]]*version[[:space:]]*=/ {
            version = $2
            next
        }
        END {
            if (!found && pkg == wanted && version != "") {
                print version
            }
        }
    ' Cargo.lock
}

check_versions() {
    local extension_version root_version
    extension_version="$(package_version extension.toml)"
    root_version="$(package_version Cargo.toml)"
    [ -n "${extension_version}" ] || fail "could not parse version from extension.toml"
    [ -n "${root_version}" ] || fail "could not parse version from Cargo.toml"

    [ "${extension_version}" = "${root_version}" ] \
        || fail "extension.toml version (${extension_version}) != Cargo.toml version (${root_version})"

    # The PRODUCT version lives on the root package (zed-al), extension.toml,
    # and both shipped native binaries. The library crates (al-types,
    # al-syntax, al-bc, …) carry INDEPENDENT semver after the al-core split, so
    # they are intentionally NOT held to the product version.
    local product_toml product_name product_version product_lock
    for product_toml in crates/al-lsp/Cargo.toml crates/al-explorer/Cargo.toml; do
        [ -f "${product_toml}" ] || fail "missing product manifest ${product_toml}"
        product_name="$(package_name "${product_toml}")"
        product_version="$(package_version "${product_toml}")"
        [ -n "${product_name}" ] || fail "could not parse package name from ${product_toml}"
        [ -n "${product_version}" ] || fail "could not parse version from ${product_toml}"
        [ "${product_version}" = "${extension_version}" ] \
            || fail "${product_toml} version (${product_version}) != extension.toml version (${extension_version})"
        product_lock="$(lock_version_for "${product_name}")"
        [ -n "${product_lock}" ] || fail "Cargo.lock has no package entry for ${product_name}"
        [ "${product_lock}" = "${product_version}" ] \
            || fail "Cargo.lock ${product_name} version (${product_lock}) != ${product_toml} version (${product_version})"
    done

    if [ -n "${TAG_NAME}" ]; then
        local tag="${TAG_NAME#refs/tags/}"
        local expected="v${extension_version}"
        [ "${tag}" = "${expected}" ] \
            || fail "release tag (${tag}) must match package version (${expected})"
        if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
            local tag_commit head_commit
            tag_commit="$(git rev-list -n1 "${tag}")"
            head_commit="$(git rev-parse HEAD)"
            [ "${tag_commit}" = "${head_commit}" ] \
                || fail "tag ${tag} points at ${tag_commit}, but HEAD is ${head_commit}"
        fi
    fi

    ok "versions aligned at ${extension_version}"
}

check_submodule_and_grammar_rev() {
    [ -d tree-sitter-al ] || fail "tree-sitter-al submodule directory is missing"

    local status line prefix
    status="$(git submodule status --recursive)"
    [ -n "${status}" ] || fail "git submodule status returned no submodules"
    while IFS= read -r line; do
        [ -n "${line}" ] || continue
        prefix="${line:0:1}"
        case "${prefix}" in
            " ")
                ;;
            "+")
                fail "submodule is checked out at a different commit than the superproject gitlink: ${line}"
                ;;
            "-")
                fail "submodule is not initialized: ${line}"
                ;;
            "U")
                fail "submodule has merge conflicts: ${line}"
                ;;
            *)
                fail "unexpected submodule status prefix '${prefix}': ${line}"
                ;;
        esac
    done <<< "${status}"

    local gitlink sub_head manifest_rev
    gitlink="$(git ls-files -s tree-sitter-al | awk '{print $2}')"
    [ -n "${gitlink}" ] || fail "could not read tree-sitter-al gitlink from index"
    sub_head="$(git -C tree-sitter-al rev-parse HEAD)"
    [ "${gitlink}" = "${sub_head}" ] \
        || fail "tree-sitter-al HEAD (${sub_head}) != superproject gitlink (${gitlink})"

    manifest_rev="$(awk -F'"' '
        /^\[grammars\.al\]/ { in_grammar = 1; next }
        /^\[/ { in_grammar = 0 }
        in_grammar && /^[[:space:]]*rev[[:space:]]*=/ { print $2; exit }
    ' extension.toml)"
    [ -n "${manifest_rev}" ] || fail "could not parse [grammars.al].rev from extension.toml"
    [ "${manifest_rev}" = "${gitlink}" ] \
        || fail "extension.toml [grammars.al].rev (${manifest_rev}) != tree-sitter-al gitlink (${gitlink})"

    ok "tree-sitter-al gitlink, HEAD, and extension.toml grammar rev are aligned"
}

check_generated_files_exist() {
    local files=(
        tree-sitter-al/.tree-sitter-cli-version
        tree-sitter-al/grammar.js
        tree-sitter-al/src/grammar.json
        tree-sitter-al/src/keywords.c
        tree-sitter-al/src/scanner.c
        tree-sitter-al/src/parser.c
        tree-sitter-al/src/node-types.json
        tree-sitter-al/queries/highlights.scm
        tree-sitter-al/queries/folds.scm
        tree-sitter-al/queries/locals.scm
        tree-sitter-al/queries/textobjects.scm
        tree-sitter-al/queries/indents.scm
        tree-sitter-al/queries/brackets.scm
        tree-sitter-al/queries/outline.scm
        tree-sitter-al/data/keywords.json
        tree-sitter-al/data/token_classification.json
        tree-sitter-al/data/builtin_functions.json
        tree-sitter-al/data/runtime_enums.json
        crates/al-syntax/data/record_methods.json
        themes/bc-themes.json
    )
    local file
    for file in "${files[@]}"; do
        [ -s "${file}" ] || fail "required generated/synchronized artifact is missing or empty: ${file}"
    done
    grep -Eq "AUTO-GENERATED|automatically generated" tree-sitter-al/grammar.js \
        || fail "tree-sitter-al/grammar.js is missing its generated-file marker"
    grep -q "@generated by tree-sitter" tree-sitter-al/src/parser.c \
        || fail "tree-sitter-al/src/parser.c is missing its tree-sitter generated marker"
    grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' tree-sitter-al/.tree-sitter-cli-version \
        || fail "tree-sitter-al/.tree-sitter-cli-version is not an exact semantic version"
    [ -x tree-sitter-al/tests/check_cli_version.sh ] \
        || fail "tree-sitter-al/tests/check_cli_version.sh is missing or not executable"
    ok "generated/synchronized artifacts are present"
}

show_generated_drift() {
    git status --short -- languages/al themes/bc-themes.json crates/al-syntax/data/record_methods.json
    git diff --stat -- languages/al themes/bc-themes.json crates/al-syntax/data/record_methods.json
    git -C tree-sitter-al status --short -- grammar.js src queries data
    git -C tree-sitter-al diff --stat -- grammar.js src queries data
}

previous_tag_before() {
    local current="${1#refs/tags/}"
    git tag --sort=-v:refname --merged HEAD --list 'v*' \
        | grep -vxF "${current}" \
        | head -n1
}

path_matches() {
    local path="$1"
    shift
    local pattern
    for pattern in "$@"; do
        # shellcheck disable=SC2053 # RHS is intentionally a glob pattern.
        if [[ "${path}" == ${pattern} ]]; then
            return 0
        fi
    done
    return 1
}

collect_changed_files() {
    local base="$1"
    git diff --name-only "${base}..HEAD" --

    local old_sub new_sub
    old_sub="$(git ls-tree "${base}" tree-sitter-al 2>/dev/null | awk '{print $3}')"
    new_sub="$(git ls-files -s tree-sitter-al | awk '{print $2}')"
    if [ -n "${old_sub}" ] && [ -n "${new_sub}" ] && [ "${old_sub}" != "${new_sub}" ]; then
        # A normal checkout may only have the new submodule commit. Fetching the
        # branch history lets us inspect the old gitlink for generated co-change.
        git -C tree-sitter-al fetch --quiet origin || true
        if git -C tree-sitter-al cat-file -e "${old_sub}^{commit}" 2>/dev/null \
            && git -C tree-sitter-al cat-file -e "${new_sub}^{commit}" 2>/dev/null; then
            git -C tree-sitter-al diff --name-only "${old_sub}..${new_sub}" \
                | sed 's|^|tree-sitter-al/|'
        else
            warn "could not inspect tree-sitter-al diff ${old_sub}..${new_sub}; old commit not available"
        fi
    fi
}

check_generated_cochange() {
    local base="$1"
    [ -n "${base}" ] || return 0
    git rev-parse --verify "${base}^{commit}" >/dev/null \
        || fail "changed-since ref is not available: ${base}"

    # `mapfile`/`readarray` (bash >= 4) is not available on stock macOS's
    # /bin/bash 3.2, so populate the array with a portable read loop instead.
    local changed=()
    local changed_file
    while IFS= read -r changed_file; do
        changed+=("${changed_file}")
    done < <(collect_changed_files "${base}" | sort -u)
    [ "${#changed[@]}" -gt 0 ] || {
        ok "no changed files since ${base}; generated co-change guard skipped"
        return 0
    }

    local language_generator_patterns=(
        'tree-sitter-al/generator/*'
        'tree-sitter-al/generator/**'
    )
    local language_generated_patterns=(
        'tree-sitter-al/grammar.js'
        'tree-sitter-al/src/grammar.json'
        'tree-sitter-al/src/keywords.c'
        'tree-sitter-al/src/scanner.c'
        'tree-sitter-al/src/parser.c'
        'tree-sitter-al/src/node-types.json'
        'tree-sitter-al/queries/*.scm'
        'tree-sitter-al/data/*.json'
        'languages/al/*'
        'themes/bc-themes.json'
    )

    local saw_language_generator=0
    local saw_language_generated=0
    local saw_record_generator=0
    local saw_record_generated=0
    local file
    for file in "${changed[@]}"; do
        if path_matches "${file}" "${language_generator_patterns[@]}"; then
            saw_language_generator=1
        fi
        if path_matches "${file}" "${language_generated_patterns[@]}"; then
            saw_language_generated=1
        fi
        if [ "${file}" = 'crates/al-semantic/examples/export_record_methods.rs' ]; then
            saw_record_generator=1
        fi
        if [ "${file}" = 'crates/al-syntax/data/record_methods.json' ]; then
            saw_record_generated=1
        fi
    done

    if [ "${saw_language_generator}" -eq 1 ] && [ "${saw_language_generated}" -eq 0 ]; then
        fail "generator inputs changed since ${base}, but no generated grammar/query/data/theme outputs changed.
Run the generator or use --regenerate locally to prove the generated artifacts are fresh."
    fi
    if [ "${saw_record_generator}" -eq 1 ] && [ "${saw_record_generated}" -eq 0 ]; then
        fail "Record method generator changed since ${base}, but crates/al-syntax/data/record_methods.json did not.
Run 'AL_TOOL_PATH=<official-extension>/bin/<platform> make record-methods' and commit the result."
    fi

    ok "generated-source/generated-output co-change guard passed since ${base}"
}

path_state_fingerprint() {
    # Hash the actual filesystem tree, including files below the
    # `tree-sitter-al` submodule. Comparing only `git status --porcelain` is
    # insufficient in a developer worktree: a generated file can be modified
    # both before and after regeneration while its status remains the same
    # single `M` line. The superproject also sees the entire dirty submodule as
    # one gitlink, so even a root `git diff` cannot observe parser-byte changes.
    #
    # The enumerated trees are generator-owned and therefore have controlled,
    # newline-free filenames.
    {
        local requested file mode
        for requested in "$@"; do
            printf 'REQUEST\0%s\0' "${requested}"
            if [ -d "${requested}" ]; then
                while IFS= read -r file; do
                    printf 'PATH\0%s\0' "${file}"
                    if [ -L "${file}" ]; then
                        printf 'SYMLINK\0%s\0' "$(readlink "${file}")"
                    else
                        mode="file"
                        [ -x "${file}" ] && mode="executable"
                        printf '%s\0' "${mode}"
                        git hash-object --no-filters "${file}"
                    fi
                done < <(
                    find "${requested}" \( -type f -o -type l \) -print \
                        | LC_ALL=C sort
                )
            elif [ -L "${requested}" ]; then
                printf 'SYMLINK\0%s\0' "$(readlink "${requested}")"
            elif [ -f "${requested}" ]; then
                mode="file"
                [ -x "${requested}" ] && mode="executable"
                printf '%s\0' "${mode}"
                git hash-object --no-filters "${requested}"
            else
                printf 'MISSING\0'
            fi
        done
    } | git hash-object --stdin
}

check_zed_language_generated() {
    local before after
    before="$(path_state_fingerprint languages/al)"
    cargo run --manifest-path tree-sitter-al/generator/Cargo.toml --release --bin al-gen -- --zed-language-only
    after="$(path_state_fingerprint languages/al)"
    if [ "${before}" != "${after}" ]; then
        git status --short -- languages/al
        git diff -- languages/al
        fail "languages/al changed while running the generator. Run \`make language\` and commit the generated output."
    fi
    ok "languages/al is generated and current"
}

check_regenerate_clean() {
    [ "${REGENERATE}" -eq 1 ] || return 0
    local paths=(
        tree-sitter-al/grammar.js tree-sitter-al/src tree-sitter-al/queries
        tree-sitter-al/data themes/bc-themes.json languages/al
    )
    local before after
    before="$(path_state_fingerprint "${paths[@]}")"
    make grammar
    after="$(path_state_fingerprint "${paths[@]}")"
    if [ "${before}" != "${after}" ]; then
        show_generated_drift
        fail "make grammar changed generator-owned output; commit the regenerated artifacts"
    fi
    ok "make grammar produced no generated diff"
}

check_full_regenerate_clean() {
    [ "${FULL_REGENERATE}" -eq 1 ] || return 0
    [ -n "${AL_EXTENSION_PATH:-}" ] \
        || fail "--full-regenerate requires AL_EXTENSION_PATH to name a pinned unpacked Microsoft AL extension"
    [ -d "${AL_EXTENSION_PATH}" ] \
        || fail "AL_EXTENSION_PATH is not a directory: ${AL_EXTENSION_PATH}"
    command -v tree-sitter >/dev/null \
        || fail "--full-regenerate requires the tree-sitter CLI on PATH"
    command -v dotnet >/dev/null \
        || fail "--full-regenerate requires dotnet for the Microsoft Record method catalog"
    tree-sitter-al/tests/check_cli_version.sh

    # Restrict the before/after snapshot to generator-owned output. This keeps
    # the check useful in a developer worktree with unrelated edits while
    # still failing if full extraction changes a committed artifact.
    local paths=(
        tree-sitter-al/grammar.js tree-sitter-al/src tree-sitter-al/queries
        tree-sitter-al/data themes/bc-themes.json languages/al
    )
    local before after
    before="$(path_state_fingerprint "${paths[@]}")"
    # `--test` invokes the full proprietary-input generator, builds the generated
    # parser, runs focused valid/invalid fixtures, and then parses every pinned
    # external corpus repository. The ordinary repository runner deliberately
    # uses `--repo-tests-only` so it remains self-contained.
    cargo run --release --manifest-path tree-sitter-al/generator/Cargo.toml --bin al-gen -- --test
    after="$(path_state_fingerprint "${paths[@]}")"
    if [ "${before}" != "${after}" ]; then
        show_generated_drift
        fail "full grammar regeneration changed generator-owned output; commit the regenerated artifacts"
    fi

    local code_analysis_dll
    code_analysis_dll="$(
        find "${AL_EXTENSION_PATH}/bin" -type f \
            -name Microsoft.Dynamics.Nav.CodeAnalysis.dll -print -quit 2>/dev/null
    )"
    [ -n "${code_analysis_dll}" ] \
        || fail "Microsoft.Dynamics.Nav.CodeAnalysis.dll was not found below AL_EXTENSION_PATH/bin"
    AL_TOOL_PATH="$(dirname "${code_analysis_dll}")" make check-record-methods

    ok "full AL-extension regeneration, Record catalog, and pinned external corpus produced no diff or failures"
}

repo_slug() {
    if [ -n "${GITHUB_REPOSITORY:-}" ]; then
        echo "${GITHUB_REPOSITORY}"
        return 0
    fi
    local url
    url="$(git remote get-url origin 2>/dev/null || true)"
    url="${url%.git}"
    url="${url%/}"
    case "${url}" in
        https://github.com/*) echo "${url#https://github.com/}" ;;
        http://github.com/*) echo "${url#http://github.com/}" ;;
        git@github.com:*) echo "${url#git@github.com:}" ;;
        *) return 1 ;;
    esac
}

require_ci_success() {
    [ "${REQUIRE_CI}" -eq 1 ] || return 0
    command -v gh >/dev/null || fail "--require-ci needs the GitHub CLI (gh)"
    local token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
    [ -n "${token}" ] || fail "--require-ci needs GH_TOKEN or GITHUB_TOKEN"
    export GH_TOKEN="${token}"

    local repo sha deadline discovery_deadline discovered now rows status conclusion url
    repo="$(repo_slug)" || fail "could not determine GitHub repository slug for --require-ci"
    sha="$(git rev-parse HEAD)"
    deadline=$(( $(date +%s) + CI_TIMEOUT_SECONDS ))
    discovery_deadline=$(( $(date +%s) + CI_DISCOVERY_TIMEOUT_SECONDS ))
    discovered=0

    echo "Waiting for successful ${CI_WORKFLOW} run on ${sha} in ${repo}..."
    while true; do
        rows="$(gh run list \
            --repo "${repo}" \
            --workflow "${CI_WORKFLOW}" \
            --commit "${sha}" \
            --limit 20 \
            --json databaseId,status,conclusion,url,headSha \
            --jq ".[] | select(.headSha == \"${sha}\") | [.databaseId, .status, (.conclusion // \"\"), .url] | @tsv")"

        if [ -n "${rows}" ]; then
            discovered=1
            while IFS=$'\t' read -r _ status conclusion url; do
                case "${status}:${conclusion}" in
                    completed:success)
                        ok "${CI_WORKFLOW} succeeded for ${sha}: ${url}"
                        return 0
                        ;;
                esac
            done <<< "${rows}"

            if ! grep -Eq $'\t(queued|in_progress|waiting|requested)\t' <<< "${rows}"; then
                echo "${rows}" >&2
                fail "found ${CI_WORKFLOW} run(s) for ${sha}, but none succeeded"
            fi
        fi

        now="$(date +%s)"
        if [ "${discovered}" -eq 0 ] && [ "${now}" -ge "${discovery_deadline}" ]; then
            fail "no ${CI_WORKFLOW} run was found for ${sha} in ${repo} after ${CI_DISCOVERY_TIMEOUT_SECONDS}s. \
This usually means the tagged commit never triggered CI: ci.yml only runs on push to main/dev, so a tag \
pushed on a commit that isn't (yet) on one of those branches will never get a run. Push the commit to \
main/dev (e.g. merge the PR) before tagging, or verify the tagged sha with 'git log --oneline main..${sha}'. \
Set CI_DISCOVERY_TIMEOUT_SECONDS to wait longer if CI is just slow to be scheduled. Not retrying for the \
full --ci-timeout-seconds (${CI_TIMEOUT_SECONDS}s)."
        fi
        [ "${now}" -lt "${deadline}" ] \
            || fail "timed out waiting for successful ${CI_WORKFLOW} run on ${sha}"
        sleep "${CI_POLL_SECONDS}"
    done
}

if [ "${CHANGED_SINCE}" = "auto" ]; then
    [ -n "${TAG_NAME}" ] || fail "--changed-since auto requires --tag"
    CHANGED_SINCE="$(previous_tag_before "${TAG_NAME}" || true)"
    if [ -z "${CHANGED_SINCE}" ]; then
        warn "no previous v* tag found; generated co-change guard has no release baseline"
    fi
elif [ -z "${CHANGED_SINCE}" ] && [ -n "${TAG_NAME}" ]; then
    CHANGED_SINCE="$(previous_tag_before "${TAG_NAME}" || true)"
fi

check_versions
check_submodule_and_grammar_rev
check_generated_files_exist
check_zed_language_generated
check_generated_cochange "${CHANGED_SINCE}"
check_regenerate_clean
check_full_regenerate_clean
require_ci_success

ok "release hygiene checks passed"
