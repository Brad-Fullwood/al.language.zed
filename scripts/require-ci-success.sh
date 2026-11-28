#!/usr/bin/env bash
# require-ci-success.sh: waits for a successful CI push run on the commit at HEAD.
#
# check-release-hygiene.sh runs this with --require-ci. Without REQUIRE_CI=1 it exits 0.
# The caller passes CI_WORKFLOW, CI_TIMEOUT_SECONDS, CI_POLL_SECONDS and
# CI_DISCOVERY_TIMEOUT_SECONDS. This script sets no defaults for them.
#
# CI runs on push and on pull_request to main and dev. A pull_request run checks out the
# merge ref, not the tagged tree, so only a push run on main or dev counts. The tagged
# commit must also be an ancestor of origin/dev or origin/main.

set -euo pipefail

if [ "${REQUIRE_CI:-0}" != "1" ]; then
    exit 0
fi

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

ok() {
    echo "OK: $*"
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
    command -v gh >/dev/null || fail "--require-ci needs the GitHub CLI (gh)"
    local token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
    [ -n "${token}" ] || fail "--require-ci needs GH_TOKEN or GITHUB_TOKEN"
    export GH_TOKEN="${token}"

    local repo sha deadline discovery_deadline discovered now rows status conclusion url
    repo="$(repo_slug)" || fail "could not determine GitHub repository slug for --require-ci"
    sha="$(git rev-parse HEAD)"
    if ! git merge-base --is-ancestor HEAD refs/remotes/origin/dev 2>/dev/null \
        && ! git merge-base --is-ancestor HEAD refs/remotes/origin/main 2>/dev/null; then
        fail "the tagged commit ${sha} is not on dev or main. Merge it first, then tag the merge result."
    fi
    deadline=$(( $(date +%s) + CI_TIMEOUT_SECONDS ))
    discovery_deadline=$(( $(date +%s) + CI_DISCOVERY_TIMEOUT_SECONDS ))
    discovered=0

    echo "Waiting for successful ${CI_WORKFLOW} push run on ${sha} in ${repo}..."
    while true; do
        rows="$(gh run list \
            --repo "${repo}" \
            --workflow "${CI_WORKFLOW}" \
            --commit "${sha}" \
            --event push \
            --limit 20 \
            --json databaseId,status,conclusion,url,headSha,headBranch,event \
            --jq ".[] | select(.headSha == \"${sha}\" and .event == \"push\" and (.headBranch == \"main\" or .headBranch == \"dev\")) | [.databaseId, .status, (.conclusion // \"\"), .url] | @tsv")"

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
CI runs on push and on pull_request to main and dev, but only a push run on main or dev counts for a \
release. This usually means the commit has no push run on main or dev yet. Set \
CI_DISCOVERY_TIMEOUT_SECONDS to wait longer if CI is just slow to be scheduled. Not retrying for the \
full --ci-timeout-seconds (${CI_TIMEOUT_SECONDS}s)."
        fi
        [ "${now}" -lt "${deadline}" ] \
            || fail "timed out waiting for successful ${CI_WORKFLOW} run on ${sha}"
        sleep "${CI_POLL_SECONDS}"
    done
}

require_ci_success
