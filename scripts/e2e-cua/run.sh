#!/usr/bin/env bash
# Run the magic-carpet-chat e2e suite on the cua box, from the Mac.
#
#   scripts/e2e-cua/run.sh <run-id|path-to-binary|installed> [suite flags]
#
#   <run-id>      a GitHub Actions run; downloads its magic-carpet-chat-linux-x86_64 artifact
#   <path>        a Linux x86_64 binary (or a .tar.gz holding one)
#   installed     keep the e2e binary already in the mc container
#                 (/usr/local/bin/magic-carpet-chat-e2e; the demo's binary is never touched)
#   suite flags   --no-paid, --only A,G, --mock-jev (development only)
#
# The box never compiles and never needs a GitHub token. Exits nonzero if any
# scenario fails. The report, frames, recordings and logs land in
# /tmp/mc-e2e/<timestamp>/.
set -euo pipefail
BOX=${MC_E2E_BOX:-magic-carpet-cua.exe.xyz}
ARTIFACT=magic-carpet-chat-linux-x86_64
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
src=${1:?usage: run.sh <run-id|path-to-binary|installed> [--no-paid] [--only A,B] [--mock-jev]}
shift
TS=$(date +%Y%m%d-%H%M%S)
OUT=/tmp/mc-e2e/$TS
mkdir -p "$OUT"

bin=""
if [[ $src == installed ]]; then
  :
elif [[ -f $src ]]; then
  bin=$src
elif [[ $src =~ ^[0-9]+$ ]]; then
  echo "downloading $ARTIFACT from run $src"
  (cd "$REPO" && gh run download "$src" -n "$ARTIFACT" -D "$OUT/artifact")
  bin=$(find "$OUT/artifact" -type f -name magic-carpet-chat | head -1)
else
  echo "not a run id, a file, or 'installed': $src" >&2; exit 2
fi
if [[ -n $bin ]]; then
  if [[ $bin == *.tar.gz || $bin == *.tgz ]]; then
    mkdir -p "$OUT/unpacked" && tar -xzf "$bin" -C "$OUT/unpacked"
    bin=$(find "$OUT/unpacked" -type f -name magic-carpet-chat | head -1)
  fi
  [[ -z $bin ]] && { find "$OUT" -type f -maxdepth 3; echo "no magic-carpet-chat binary found" >&2; exit 2; }
  file "$bin" | grep -q 'ELF 64-bit.*x86-64' || { file "$bin"; echo "not a Linux x86_64 binary" >&2; exit 2; }
  echo "installing $(shasum -a 256 "$bin" | cut -c1-16) into mc"
  ssh "$BOX" 'mkdir -p ~/mc/e2e-bin'
  scp -q "$bin" "$BOX:mc/e2e-bin/magic-carpet-chat"
  # Like install-release.sh, but beside the demo's binary, never over it.
  ssh "$BOX" 'set -e
    docker cp ~/mc/e2e-bin/magic-carpet-chat mc:/usr/local/bin/magic-carpet-chat-e2e
    docker exec mc chmod +x /usr/local/bin/magic-carpet-chat-e2e'
fi

rsync -a --exclude runs/ --exclude __pycache__/ "$HERE/" "$BOX:mc/e2e/"
echo "running the suite on $BOX (run $TS)"
# Detached on the box, so a dropped ssh session never kills a run halfway
# (least of all P). The Mac follows the log and reconnects until the exit
# file appears.
R="mc/e2e/runs/$TS"
ssh "$BOX" "mkdir -p ~/$R && cd ~/mc/e2e && (setsid nohup sh -c '~/mc/.venv/bin/python -u suite.py --run-id $TS $*; echo \$? > ~/$R/exit' > ~/$R/suite.out 2>&1 < /dev/null &)"
: > "$OUT/suite.out"
# Poll, never `tail -f`: a follow over ssh can outlive the run.
while :; do
  seen=$(wc -l < "$OUT/suite.out" | tr -d ' ')
  # The exit file is checked before the tail, so the last lines are never missed.
  new=$(ssh -o ConnectTimeout=20 "$BOX" "if test -f ~/$R/exit; then echo __E2E_DONE__; elif ! pgrep -f 'suite[.]py --run-id $TS' >/dev/null; then echo 4 > ~/$R/exit; echo __E2E_DONE__; fi; tail -n +$((seen + 1)) ~/$R/suite.out" 2>/dev/null) || { sleep 10; continue; }
  body=$(printf '%s\n' "$new" | sed '/^__E2E_DONE__$/d')
  [[ -n $body ]] && printf '%s\n' "$body" | tee -a "$OUT/suite.out"
  grep -q '^__E2E_DONE__$' <<<"$new" && break
  sleep 10
done
code=$(ssh "$BOX" "cat ~/$R/exit")
rsync -a "$BOX:mc/e2e/runs/$TS/" "$OUT/" || echo "could not copy the run back" >&2

echo
echo "report:     $OUT/report.md"
echo "recordings: $OUT/<scenario>/<scenario>.mp4"
[[ -f $OUT/report.md ]] && cat "$OUT/report.md"
exit "$code"
