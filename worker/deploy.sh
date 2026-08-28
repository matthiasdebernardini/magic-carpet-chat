#!/bin/sh
# Deploy the chat worker to the celld fleet on nashauto-git.exe.xyz.
#
#   ANTHROPIC_API_KEY=sk-ant-… ./deploy.sh            # deploy
#   ANTHROPIC_API_KEY=sk-ant-… ./deploy.sh --dry-run  # bundle only, write nothing
#
# The /chat prefix is the whole safety story. A deploy into the bucket root
# rewrites deploy/current.json and points the dgit node at this worker the next
# time it restarts, which 404s every hosted repository.
set -eu

: "${ANTHROPIC_API_KEY:?export ANTHROPIC_API_KEY first}"
BUCKET="${BUCKET:-s3://nashauto-git-520235901794/chat}"

cd "$(dirname "$0")"
# The X's must end the template. GNU mktemp reads a tail after them as a suffix
# and randomises anyway, but BSD mktemp — the one on a Mac — takes the whole name
# literally: "wrangler.deploy.XXXXXX.jsonc" was the file, every run, with the key
# in it. The copy stays in this directory because celld reads "main" relative to
# the config it is given.
config=$(mktemp wrangler.deploy.jsonc.XXXXXX)
trap 'rm -f "$config"' EXIT INT TERM
chmod 600 "$config"
sed "s|@ANTHROPIC_API_KEY@|$ANTHROPIC_API_KEY|" wrangler.celld.jsonc > "$config"

eval "$(aws configure export-credentials --format env)"
export AWS_REGION=us-east-1
celld deploy --config "$config" --bucket "$BUCKET" "$@"

case " $* " in *" --dry-run "*) exit 0 ;; esac

cat <<'NEXT'

Deployed to the bucket. celld nodes load a deployment at startup, so the box
still serves the old version until its own unit restarts:

  ssh exedev@nashauto-git.exe.xyz 'sudo systemctl restart celld-chat'

Never restart celld.service — that one is dgit.
NEXT
