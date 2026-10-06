---
title: "Automated app test cycles with Cua, Jev, Claude Code and Cap"
subtitle: "A desktop app, a Linux sandbox, an evidence-based decision loop, a coding agent that closes the loop, and a recording you can send to someone."
---

## What you end up with

One command runs your desktop app through a real user journey on a real screen. Jev picks every UI action from a candidate table and judges every postcondition from what the screen shows. When a step fails, Claude Code reads the log, fixes the app, ships a release, and reruns. Cap records the run so a cofounder can watch it.

The example below is a plain app with a list and a form: open the list, open the "New item" form, fill it, submit, and see the item appear. Swap in your own screens.

## The parts

| Part | Role | What it needs |
|---|---|---|
| Cua (trycua) | A Linux desktop in a container, with a screenshot, mouse, keyboard and shell API. | Docker. Image `public.ecr.aws/k5j5w0x5/cua-ubuntu-24.04:docker-latest`. |
| Jev (TypeSafe) | Answers two questions per step: which candidate to run, and whether the postcondition holds. Returns a probability, never free text. | `TYPESAFE_API_KEY`, the `typesafe-sdk` Python package. |
| Claude Code | Authors the candidate tables, drives the box over ssh, reads the run logs, fixes the app, tags the release, reruns. | A goal prompt with the human handoffs marked. |
| Cap | Records the run, exports mp4, uploads to a share link. | `cap` CLI on the Mac. Inside the container, `ffmpeg -f x11grab`. |
| echt (optional) | Keeps the chain: prompt, commit, run log, crash. | A Sentry DSN for the app and an ingest key for the loop. |

## The loop

One step is a goal, a candidate table and a postcondition.

```python
Step(
  goal="Open the New item form.",
  cands=[NEW_FORM, REFRESH],
  done="A form titled 'New item' is open.",
  cues=("New item", "Cancel"),
)
```

The runner appends three candidates the app does not own: `already-done` (the screen already shows the goal), `reobserve` (wait and look again) and `abstain` (nothing here is safe). Jev picks one id. An unknown id halts the run. So does a pick under 0.5 confidence. After the action settles, Jev scores the postcondition from a fresh screenshot. Under 0.6 it tries again, three times at most, then halts.

The screen reaches Jev as text: Tesseract reads a grayscale screenshot upscaled two times, because small text on a dark UI is lost otherwise. The `cues` are phrases whose presence or absence in that text goes to Jev as evidence, so the judgment does not rest on raw OCR alone.

Every event goes to a JSONL log with an epoch timestamp: `start`, `choice`, `act`, `verify`, `halt`, `end`. That log is the whole output. Captions, echt events and the next Claude Code prompt all come from it.

## Set up the box

1. Create a box. On exe.dev: `ssh exe.dev "new --name=my-app-cua --cpu=2 --memory=4GB --disk=25GB"`. Any Linux host with Docker works.
2. Start the desktop: `docker run -d --name sandbox --restart unless-stopped --shm-size 1g <cua image>`. Publish no ports. VNC and the API stay on the Docker bridge.
3. Install the app's runtime libraries inside the container, plus `tesseract-ocr`, `xdotool` and `ffmpeg`.
4. Copy the app's release binary in. Never compile on the box: a release tag builds the Linux binary in CI, and the box downloads it.
5. On the host: `tesseract`, `jq`, and a Python venv with `cua` and `typesafe-sdk`.
6. Put secrets in files with mode 600. A step file names a secret as `$user_password`. The runner reads it from the key file at the moment it types, so it never reaches a log.
7. To watch: `tailscale up`, then `tailscale serve --bg http://172.17.0.2:6080` and open `/vnc.html?autoconnect=1` on the tailnet name.

## Write a step file

A step file is Python that imports `Step` and `Cand` and exports `STEPS`. Rules that held:

- **Candidates are complete actions.** "Type the four fields, Enter after each" is one candidate. Jev picks. It does not compose.
- **Every table has a way out.** A `cancel` candidate beside `submit`, for the form that is not the one you expected.
- **Postconditions name text the OCR can see.** "The list shows 'Item saved'", not "the save worked".
- **Focus first.** Each candidate starts with a `focus` action on the app window and parks the pointer in a corner. A hovering pointer opens tooltips that hide the header text.
- **Keyboard over mouse.** Some UI toolkits ignore synthetic mouse events, so the journey runs on keystrokes through `xdotool key`. A click that cannot be avoided gets a fixed coordinate.
- **Settle times are per step.** A submit that talks to a server needs ten seconds. Opening a form needs two.

## Run it

```
. .venv/bin/activate
python agent.py demo_open_form   # open the list, open the New item form
python agent.py demo_submit      # fill it, submit, see the item listed
```

Without `TYPESAFE_API_KEY` the loop runs in mock mode and takes the first candidate blindly. Mock checks that the actions land. Live checks that the app is right.

Read the log with the halt in view:

```
jq -c 'select(.event=="halt" or .event=="verify") | {event, step, reason, p, screen}' runs/<run>.jsonl
```

A `verify` with a low `p` and its OCR text says what the screen showed instead of what you expected. That is the bug report.

## Close the cycle with Claude Code

Give Claude Code one goal prompt with the outcome, the rules and the steps, and mark every human handoff with **ME** (a login URL to approve, a key to paste). It builds the box, writes the step files, runs the loop, and stops at the handoffs.

When a step halts, the log is the prompt. Claude Code reads the OCR text, maps it to the screen in the source, changes the app, runs the tests, tags a release, installs the CI binary on the box, and reruns from `FROM_STEP=<n>`. Our first failing cycle found a toast so large it covered the card it announced. The fix shipped in the next tag.

Two habits keep the cycle honest. Keep `already-done` in every table, because a rerun starts from a screen that may already show the goal. Verify side effects outside the UI, in the database or the API, because the screen can say "saved" and be wrong.

## Record it with Cap

Start the recorder inside the container before the first step, `ffmpeg -f x11grab -i :1 -r 10 /tmp/demo.mp4`, and stop it after the last. Copy it out with `docker cp`, then `cap upload --json` on the Mac gives the share link.

The run log is the caption track. Each event has an epoch time, and the video starts at the file's mtime minus its duration, so every caption lands where the screen changed. Cut the dead start: two already-satisfied steps cost 27 seconds of nothing. Burn captions in with ffmpeg's `subtitles` filter on the box, which has libass. Narration is one ElevenLabs call per caption, mixed with `adelay` and `amix`.

## Keep the chain in echt

Point the app at a Sentry DSN from `echt token new --sentry --project <name>`, and the stock Sentry SDK reports panics and surfaced errors. Ship each run log as a `log` event with a signed POST to `/ingest/v1/events`. Claude Code's session already lands there through hooks. A crash, the run that triggered it, the prompt that fixed it and the commit that shipped it become one chain, and `echt brain context --commit HEAD` walks it.

## Gotchas we paid for

- Two async runtimes in one process do not mix. Keep the UI's executor and the network runtime on separate threads.
- Sentry's transport builds TLS at init, before the app installs its own crypto provider. Install the provider first thing in `main`.
- The container has no MagicDNS. Pin the tailnet hostname in its `/etc/hosts` before the app dials it.
- Homebrew's ffmpeg has no `subtitles` filter. Burn captions on the Linux box.
- libass scales the SRT font to a 288-line canvas. Size 9 is readable at 1440 by 1080. Size 20 covers the screen.
- The recording is real time. A long wait after submit is the server, not the loop.
