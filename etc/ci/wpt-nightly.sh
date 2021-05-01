#!/usr/bin/env bash

# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

set -o errexit
set -o nounset
set -o pipefail

REMOTE_NAME=sync-fork

LOG_FILE=test-wpt.log
BLUETOOTH_LOG_FILE=test-wpt-bluetooth.log
WDSPEC_LOG_FILE=test-wpt-wdspec.log
CURRENT_DATE=$(date +"%d-%m-%Y")
BRANCH_NAME="wpt_update"
REMOTE_BRANCH_NAME="wpt_update_${CURRENT_DATE}"

export GIT_AUTHOR_NAME="WPT Sync Bot"
export GIT_AUTHOR_EMAIL="josh+wptsync@joshmatthews.net"
export GIT_COMMITTER_NAME="${GIT_AUTHOR_NAME}"
export GIT_COMMITTER_EMAIL="${GIT_AUTHOR_EMAIL}"

# Remove all local traces of this sync operation.
function cleanup() {
    git remote rm "${REMOTE_NAME}" || true
    git reset --hard || true
    git checkout master || true
    git branch -D "${BRANCH_NAME}" || true
    ./mach update-wpt --abort || true
}

# Using an existing log file, update the expected test results and amend the
# last commit with the new results.
function unsafe_update_metadata() {
    ./mach update-wpt "${1}" || return 1
    # Hope that any test result changes from layout-2013 are also applicable to layout-2020.
    ./mach update-wpt --layout-2020 "${1}" || return 2
    # Ensure any new directories or ini files are included in these changes.
    git add tests/wpt/metadata tests/wpt/metadata-layout-2020 tests/wpt/mozilla/meta || return 3
    # Merge all changes with the existing commit.
    git commit -a --amend --no-edit || return 3
}

# Push the branch to a remote branch, then open a PR for the branch
# against servo/servo.
function unsafe_open_pull_request() {
    WPT_SYNC_USER=servo-wpt-sync

    # If the branch doesn't exist, we'll silently exit. This deals with the
    # case where an earlier step either failed or discovered that syncing
    # is unnecessary.
    git checkout "${BRANCH_NAME}" || return 0

    if [[ -z "${WPT_SYNC_TOKEN+set}" && "${TASKCLUSTER_PROXY_URL+set}" == "set" ]]; then
        SECRET_RESPONSE=$(curl ${TASKCLUSTER_PROXY_URL}/api/secrets/v1/secret/project/servo/wpt-sync)
        WPT_SYNC_TOKEN=`echo "${SECRET_RESPONSE}" | jq --raw-output '.secret.token'`
    fi

    if [[ -z "${WPT_SYNC_TOKEN+set}" ]]; then
        echo "Github auth token missing from WPT_SYNC_TOKEN."
        return 1
    fi

    # Push the changes to a remote branch owned by the bot.
    # AUTH="${WPT_SYNC_USER}:${WPT_SYNC_TOKEN}"
    # UPSTREAM="https://${AUTH}@github.com/${WPT_SYNC_USER}/servo.git"
    # git remote add "${REMOTE_NAME}" "${UPSTREAM}" || return 2
    git push -f "${REMOTE_NAME}" "${BRANCH_NAME}:${REMOTE_BRANCH_NAME}" &>/dev/null || return 3

    # Prepare the pull request metadata.
    BODY="Automated downstream sync of changes from upstream as of "
    BODY+="${CURRENT_DATE}.\n"
    BODY+="[no-wpt-sync]\n"
    BODY+="r? @servo-wpt-sync\n"
    cat <<EOF >prdata.json || return 4
{
  "title": "Sync WPT with upstream (${CURRENT_DATE})",
  "head": "${WPT_SYNC_USER}:${REMOTE_BRANCH_NAME}",
  "base": "master",
  "body": "${BODY}",
  "maintainer_can_modify": true
}
EOF

    # Open a pull request using the new branch.
    OPEN_PR_RESPONSE=$(curl -H "Authorization: token ${WPT_SYNC_TOKEN}" \
                            -H "Content-Type: application/json" \
                            --data @prdata.json \
                            https://api.github.com/repos/servo/servo/pulls) || return 5

    echo "${OPEN_PR_RESPONSE}" | \
        jq '.review_comments_url' | \
        sed 's/pulls/issues/' | \
        xargs curl -H "Authorization: token ${WPT_SYNC_TOKEN}" \
                   --data "{\"body\":\"@bors-servo r+\"}" || return 6
}

function update_metadata() {
    unsafe_update_metadata "${1}" || { code="${?}"; cleanup; return "${code}"; }
}

function open_pull_request() {
    unsafe_open_pull_request || { code="${?}"; cleanup; return "${code}"; }
}

SCRIPT_NAME="${0}"

function main() {
    for n in {1..20}
    do
      code=""
      update_metadata "wpt${n}-logs-linux/test-wpt.${n}.log" || code="${?}"
      if [[ "${code}" != "" ]]; then
        return "${code}"
      fi
    done

    open_pull_request
    cleanup
}

# Ensure we clean up after ourselves if this script is interrupted.
trap 'cleanup' SIGINT SIGTERM
main
