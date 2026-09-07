# Goal: read and merge kind-0 profiles without losing fields

## Context

`src/nostr.rs` reads each account's kind-0 (the Nostr profile event) and,
after a Coinos signup, republishes it with only `lud16` changed. The current
working tree routes both paths through `nostr_sdk::Metadata`
(`parse_profile` at line 568, `merge_lud16` at 597, `publish_lud16` at 616).

`Metadata::from_json` is strict: one known field of the wrong type fails the
whole parse. A Codex review (GPT-6 Astra, 2026-09-07) found three
regressions against HEAD, which read fields one by one from a
`serde_json::Value`:

1. `{"name":"Alice","about":42}` reads as an empty profile. The UI hides
   Alice's name and lud16 and offers a Coinos wallet she does not need.
2. `{"name":"Alice","lud16":123}` cannot be merged, so the wallet exists but
   every "Retry publishing" fails. HEAD overwrote the bad value and kept the
   rest.
3. `{"about":null}` republishes without the `about` key. Minor, same root.

The same nsec may live in a phone app or browser extension, so this app is
never the only writer of the profile. Merge, never rebuild.

## Outcome

Profile reading takes each field on its own, and merging changes one key
and nothing else. `Metadata` is no longer used on the profile path.

## Tasks

1. `parse_profile`: return a small private struct (name, picture, lud16) read
   from `serde_json::Value`. Each field is taken independently:
   `display_name` then `name`; a non-string or blank value counts as absent.
   Unparseable content is an empty profile. Keep `profile_loaded` as the one
   place that builds `Update::ProfileLoaded`.
2. `merge_lud16`: parse the existing content as `serde_json::Value`. An
   object gets `lud16` set to the new string, every other key untouched,
   `null` values included. `None` or blank content starts from `{}`. Any
   non-object stays an error with the current message. Return the
   `serde_json::Map`, not `Metadata`.
3. `publish_lud16`: serialize that map as the event content and return it.
   `create_coinos_wallet` then builds `ProfileLoaded` from the map through
   the task 1 reader, with `lud16` set to the new address.
4. Tests in `nostr::tests`:
   - `merging_lud16_keeps_every_other_profile_field` (line 1373): rewrite
     against the map. Add the three cases above: wrong-typed `about`
     survives, wrong-typed `lud16` is replaced, `null` values survive.
   - one reading test: `{"name":"Alice","about":42}` yields name Alice;
     `{"display_name":"","name":"A"}` yields A.
5. Update the doc comments on `merge_lud16` and `parse_profile` to say
   "field by field" and drop the `Metadata` mentions.

## Acceptance criteria

- `rg -n Metadata src/nostr.rs` matches only `Kind::Metadata`.
- `mbx clippy --all-targets` is clean and `mbx nextest run` passes,
  including the new cases.
- A `git diff HEAD -- src/nostr.rs` shows no change outside the profile
  functions, their callers' `ProfileLoaded` construction, and tests.

## Constraints

- No new dependencies. `serde_json` is already in the tree.
- Leave `src/secrets.rs` alone; the cached dev-dir error is accepted.
- Do not touch the wallet, shell, or onboarding modules.
- Use `mbx`, never bare `cargo build`.
