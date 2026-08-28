# Chat backend worker

The server half of Magic Carpet Chat. It runs on [celld](https://github.com/denoland/celld),
a self-hosted Workers plus Durable Objects runtime. The Rust desktop client at the
repository root talks to the Anthropic API directly; this worker does the same job
for clients that should not hold a key.

One Durable Object per conversation id. The cell stores the transcript in its own
SQLite table, sends the whole transcript to the Anthropic Messages API, copies the
reply stream back byte for byte, and writes the reply to the table while it
arrives.

## API

| Route | Body | Answer |
| --- | --- | --- |
| `POST /chat/:id` | `{"message": "…"}` | `text/event-stream` — the API's own SSE |
| `GET /chat/:id` | — | `{"messages": [{"role", "content", "created_at", "complete"}]}` |

`:id` is 1–128 characters of `A-Z a-z 0-9 . _ -`. Any other path is a 404.

**There is no authentication. The tailnet is the perimeter.** Put an authenticating
proxy in front of this worker before you expose it on the public internet.

An upstream failure passes through with its own status and body, and the user
message is removed again. A turn that produces no text — an error event inside the
stream, or a reply of tool use alone — is rolled back the same way. The Messages
API rejects a history whose roles do not alternate, so an unanswered user turn
would wedge the conversation for good.

The key is read from `env.ANTHROPIC_API_KEY` and is never logged.

## One turn at a time

A POST that arrives while a reply is streaming gets `409 {"error": "a reply is in
flight"}`. Without it the second turn read a history holding the first user
message and not its reply, and the transcript ended `user, user, assistant,
assistant` — which the Messages API refuses from then on.

## `complete`

The assistant row is written when the first text delta arrives and updated as the
text accumulates, at most every 250 ms or every 16 deltas. `complete` turns true
at `message_stop`.

So a reply that never finished is still there, and says so: a client that hangs up
mid-stream, an upstream that dies mid-stream, an error event. Read `complete:
false` as "this is a fragment", not as an answer.

This matters because a hangup is not recoverable from inside the worker. The
client cancels, the runtime destroys that request's whole execution context, and
the relay stops mid-`await` — no `catch`, no `finally`, no `abort` event, and
`ctx.waitUntil` does not hold it open. Only what is already on disk survives.

That is also why the in-flight flag has a clock. The flag cannot be cleared by the
dead relay, so a turn that has taken no byte from the API for 30 seconds loses its
claim to the next POST, and a POST drops a trailing unanswered user message before
adding its own. The cell cannot wedge.

## Local loop

```sh
npm install
printf 'ANTHROPIC_API_KEY="sk-ant-…"\n' > .dev.vars   # gitignored
npm run dev                                            # 127.0.0.1:8787
```

```sh
curl -N -X POST http://127.0.0.1:8787/chat/demo \
  -H 'content-type: application/json' -d '{"message":"say hi"}'
curl -s http://127.0.0.1:8787/chat/demo
```

To answer from a local stub instead of the real API, point the cell at it:
`npx wrangler dev --var ANTHROPIC_BASE_URL:http://127.0.0.1:8799`. Leave
`ANTHROPIC_BASE_URL` unset everywhere else — the cell relays the upstream error
body to the client word for word, so a stub that echoes request headers would
hand the client the key.

`wrangler dev` runs workerd, not celld, so it can run APIs celld does not have. To
exercise the real runtime, deploy to a scratch prefix and run a node against it —
never the bucket root, and never the `chat` prefix:

```sh
ANTHROPIC_API_KEY=sk-ant-… BUCKET=s3://nashauto-git-520235901794/chat-dev ./deploy.sh
eval "$(aws configure export-credentials --format env)"
AWS_REGION=us-east-1 celld --bucket s3://nashauto-git-520235901794/chat-dev
```

`npm run typecheck` runs `tsc --noEmit`.

## Deploy

```sh
ANTHROPIC_API_KEY=sk-ant-… ./deploy.sh          # add --dry-run to bundle only
```

The script bundles `wrangler.celld.jsonc` with the key substituted into a
temporary copy, then writes the deployment under the `chat` prefix of
`s3://nashauto-git-520235901794`. celld has no secrets, only string vars, so the
key travels in the deployed bundle metadata — it must never be written into a
tracked file.

The prefix is the whole safety story. That bucket also holds dgit, the nashcode
forge. A deploy into the bucket **root** rewrites the fleet-wide
`deploy/current.json` and points dgit's node at this worker the next time it
restarts, which 404s every hosted repository.

A celld node loads its deployment at startup, so the box keeps serving the old
version until its own unit restarts:

```sh
ssh exedev@nashauto-git.exe.xyz 'sudo systemctl restart celld-chat'
```

Never restart `celld.service`. That one is dgit.

Never rename the script. Durable Object data is bound to the script name, so a
rename orphans every stored conversation.

## Box setup, once

The box runs a second celld daemon beside dgit's. Both defaults for memory and
disk assume one node per machine, so the chat unit pins its own:

`/etc/systemd/system/celld-chat.service`

```ini
[Unit]
Description=celld node (magic-carpet-chat)
After=network-online.target
Wants=network-online.target

[Service]
User=exedev
EnvironmentFile=/home/exedev/chat.env
Environment=CELLD_WATCH=/home/exedev/chat-state
Environment=CELLD_V8_HEAP_LIMIT_MB=512
Environment=CELLD_MAX_RSS_MB=600
Environment=CELLD_LOCAL_CACHE_MAX_BYTES=536870912
Environment=CELLD_SHUTDOWN_DRAIN_MS=15000
ExecStart=/home/exedev/.local/bin/celld --bucket s3://nashauto-git-520235901794/chat --region us-east-1 --listen 127.0.0.1:8081
Restart=always
RestartSec=2
KillSignal=SIGTERM
TimeoutStopSec=25

[Install]
WantedBy=multi-user.target
```

`/home/exedev/chat.env`, mode 0600, holds `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, `AWS_REGION=us-east-1`, and
`CELLD_BUCKET=s3://nashauto-git-520235901794/chat`.

```sh
sudo systemctl daemon-reload && sudo systemctl enable --now celld-chat
sudo tailscale serve --bg --https=10000 http://127.0.0.1:8081
```

Ports 443 and 8443 already carry dgit and the nashcode viewer; 10000 is the third
HTTPS port Tailscale allows. Do not run `nashcode setup` to do any of this — its
template overwrites the hand-written `celld.service` and bounces dgit.
