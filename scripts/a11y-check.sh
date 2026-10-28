#!/usr/bin/env bash
# Checks the accessibility ids the e2e driver relies on (docs/a11y-ids.md).
#
# Launches the app with a fresh, empty store (no account, nothing is signed
# or paid), reads its accessibility tree with macos-harness, walks setup,
# Dashboard, Bounties, Chat, Wallet, a placeholder screen and Accounts by
# pressing elements, and checks each id has the expected role.
#
# macOS only. Needs macos-harness with Accessibility permission and network
# for the instance's bounty list.
#
#   scripts/a11y-check.sh [path/to/magic-carpet-chat]   # default: target/debug
set -euo pipefail

bin=$(cd "$(dirname "$0")/.." && pwd)/target/debug/magic-carpet-chat
bin=${1:-$bin}
[[ -x $bin ]] || { echo "no binary at $bin; run mbx build first" >&2; exit 2; }
command -v macos-harness >/dev/null || { echo "macos-harness is not installed" >&2; exit 2; }

store=$(mktemp -d)
# No imported keys and no crash reporting: the run must touch nothing.
env -u MC_NSECS -u MC_SENTRY_DSN MC_STORE_DIR="$store" "$bin" >"$store/app.log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true; rm -rf "$store"' EXIT
app="#pid:$pid"

failures=0
tree=""

read_tree() { tree=$(macos-harness get_app_state --app "$app" --disableDiff 2>/dev/null || true); }

# The harness line for an id: "<index> <role words> <label>, ID: <id>" (long
# labels come after the id instead).
line_of() { grep -E "ID: $1(,|$)" <<<"$tree" | head -1 || true; }

# expect <role> <id>: role is the harness's word for it (button, text,
# Heading, tab, "tab group", list, container, "text field", link).
expect() {
  local role=$1 id=$2 line
  line=$(line_of "$id")
  if [[ -z $line ]]; then
    echo "FAIL missing  $id"; failures=$((failures + 1)); return
  fi
  if [[ $role == text ]] && [[ $line =~ ^[[:space:]]*[0-9]+\ text\ (field|entry) ]]; then
    echo "FAIL role     $id is not plain text: $line"; failures=$((failures + 1)); return
  fi
  if [[ ! $line =~ ^[[:space:]]*[0-9]+\ $role[\ ,] ]]; then
    echo "FAIL role     $id is not '$role': $line"; failures=$((failures + 1)); return
  fi
  echo "ok   $role $id"
}

# press <id>: click the element by its index, then re-read the tree.
press() {
  local line index
  line=$(line_of "$1")
  [[ -n $line ]] || { echo "FAIL press    $1 not found"; failures=$((failures + 1)); return; }
  index=$(sed -E 's/^[[:space:]]*([0-9]+).*/\1/' <<<"$line")
  macos-harness click --app "$app" --element_index "$index" >/dev/null \
    || { echo "FAIL press    $1: macos-harness refused"; failures=$((failures + 1)); }
  sleep 1
  read_tree
}

# wait_for <id>: poll until the id shows (window opening, network reads).
wait_for() {
  for _ in $(seq 1 20); do
    read_tree
    [[ -n $(line_of "$1") ]] && return
    sleep 1
  done
}

echo "== account setup"
wait_for setup-create
expect Heading setup-title-choose
expect button setup-create
expect button setup-paste
expect button setup-browse

press setup-create
expect Heading setup-title-create
expect "text field" setup-name
expect button setup-create-go
expect button setup-back
if [[ -n $(line_of setup-name) ]]; then
  index=$(sed -E 's/^[[:space:]]*([0-9]+).*/\1/' <<<"$(line_of setup-name)")
  macos-harness set_value --app "$app" --element_index "$index" --value "a11y check" >/dev/null || true
  read_tree
fi
if [[ $(line_of setup-name) == *"Value: a11y check"* ]]; then
  echo "ok   typed into setup-name"
else
  echo "FAIL typed text not in setup-name"; failures=$((failures + 1))
fi
press setup-back
press setup-browse

echo "== shell and dashboard"
wait_for nav-dashboard
expect button rail-logo
expect button rail-add-account
expect tab rail-accounts
expect tab nav-dashboard
[[ $(line_of nav-dashboard) == *"Value: on"* ]] && echo "ok   nav-dashboard selected" \
  || { echo "FAIL nav-dashboard not selected"; failures=$((failures + 1)); }
expect "text field" top-search
expect text sidebar-open
expect Heading dash-heading
expect text dash-summary
expect list dash-attention-list
expect list dash-activity-list

echo "== bounties"
press nav-bounties
wait_for bounties-list
expect Heading bounties-title
expect "tab group" bounties-tabs
expect tab bounties-tab-house
expect list bounties-list
expect button bounties-new

echo "== chat"
press nav-chat
expect list chat-transcript
expect container chat-composer

echo "== wallet"
press nav-wallet
expect Heading wallet-title
expect text wallet-no-account

echo "== placeholder"
press nav-leaderboards
expect Heading placeholder-leaderboards-title
expect text placeholder-leaderboards

echo "== accounts"
press rail-accounts
expect Heading accounts-title
expect button accounts-add
expect list accounts-list

if ((failures)); then
  echo "$failures check(s) failed; last lines of the app log:"
  tail -20 "$store/app.log"
  exit 1
fi
echo "all checks passed"
