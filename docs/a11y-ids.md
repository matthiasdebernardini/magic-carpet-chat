# Accessibility ids

The contract between the app and the e2e driver. Every id below is an
AccessKit `author_id`: `AXIdentifier` on macOS, `AccessibleId` on AT-SPI
(Linux). The driver finds a control by its id, reads its label, and presses it
through the accessibility tree instead of screenshots.

`scripts/a11y-check.sh` launches the app with an empty store and checks a core
set of these ids with macos-harness.

## Reading the tree

- **macOS.** `macos-harness get_app_state --app <app>` prints one line per
  node: `<index> <role> <label>, ID: <id>` (a long label comes after the id).
  Click with `--element_index`; type with `set_value` or `type_text`.
- **Roles as macos-harness prints them.**

  | AccessKit role | macos-harness |
  |---|---|
  | Button | `button` |
  | Label | `text` |
  | Heading | `Heading` |
  | Link | `link` |
  | Tab | `tab (settable, boolean) <label>, Value: on/off` |
  | TabList | `tab group` |
  | List | `list` |
  | ListItem | `container (selectable)` or `container (selected)` |
  | Group, Status | `container` |
  | TextInput | `text field (settable)` |
  | multi-line TextInput | `text entry area (settable)` |
  | Image | `image` |

- **Selection.** Which screen, tab, account or bounty is current is the
  `Value: on` of a Tab, or `(selected)` on a bounty row. macOS drops
  "selected" on buttons, so nothing selectable is a Button.
- **Descriptions.** A node's description shows as `Help: …`. It carries the
  extra state a label can't: `unavailable` on an inactive step button,
  `user`/`assistant` on a chat bubble, `Claude is replying` on the transcript,
  the open-bounty count on `nav-bounties`.
- **List items fold.** macos-harness prints a ListItem as one line with its
  children's text folded in and no indexes for them. So only rows whose
  children nobody needs to address are ListItems (bounty rows, followed
  chips, chat bubbles). Rows with buttons or parts are Groups.
- **Press needs the node on screen.** gpui presses a node by clicking its
  centre and has no scroll-into-view. Scroll a control into view before
  pressing it (the Remove card at the bottom of Identity & keys, for example).
- **Empty labels.** macos-harness prints a node with an id and an empty label
  as a bare word without `ID:`. A few labels can be empty (a profile with no
  name); match those rows through their parent.
- **Nested buttons.** `sidebar-remove-account` sits inside `sidebar-account`.
  Pressing the chip clicks its centre; it opened the Trust tab in testing,
  but if the layout ever puts the Remove line at the centre the press arms
  Remove instead. Arming never deletes.
- **Disabled.** gpui 0.3.4 has no disabled flag. Step buttons say
  `unavailable` in their description; other inert controls keep their node and
  a nearby status line says why (`wallet-send` while "Sending…").
- **Labels follow the visible text**, including when it changes; the id never
  changes with it. Exceptions are listed in their tables.
- **Secrets never reach the tree.** The revealed nsec is labelled
  `Secret key`; the key input is labelled `Secret key`; Coinos passwords appear
  nowhere.
- **Row ids come from data**, not position (chat bubbles and repeated
  activity rows are the exceptions, below): `<pubkey>` is the account's
  64-char hex public key, `<bounty-id>` the instance's bounty UUID,
  `<event-id>` the claim event's hex id. Chat bubbles are the one exception
  (see Chat).

## Linux (AT-SPI)

Read from gpui-pre-linux 0.3.4 and accesskit_unix 0.22.1, and used by the e2e
suite on the cua box (`scripts/e2e-cua/atspi.py`, X11 under xfce).

- Both the X11 and the Wayland window create an `accesskit_unix::Adapter`
  (no app change or feature flag needed). It runs on its own async-io thread.
- The adapter watches `org.a11y.Status.IsEnabled` on the **session D-Bus**.
  Only when that is true does it connect to the **accessibility bus**
  (`AT_SPI_BUS_ADDRESS`, else `org.a11y.Bus.GetAddress` on the session bus),
  register the app and ask gpui for the tree.
- So a headless host needs: a session bus (`dbus-run-session`, with
  `DBUS_SESSION_BUS_ADDRESS` exported to the app), at-spi2-core
  (`at-spi-bus-launcher` and `at-spi2-registryd`), and accessibility switched
  on: `busctl --user set-property org.a11y.Bus /org/a11y/bus org.a11y.Status
  IsEnabled b true` (or `gsettings set org.gnome.desktop.interface
  toolkit-accessibility true`). The app can start before or after the switch.
- The ids are AT-SPI `AccessibleId` (at-spi2-core 2.34 or later;
  `Atspi.Accessible.get_accessible_id()` in pyatspi/gi).
- AT-SPI names a Label from its value, not its label. The app gives every
  Label its text as value too, on Linux only (`src/a11y.rs`; on macOS a value
  makes a Label settable text). A build before that change reads every Label
  as an empty name; other roles are named from their label.
- Press with the Action interface (`do_action`, "click"), not coordinates:
  under Wayland the app cannot know its window position, so extents are
  window-relative at best. gpui still clicks the node's centre, so the node
  must be on screen.

## Shell: rail, sidebar, top bar

| id | role | label | meaning |
|---|---|---|---|
| `rail-logo` | Button | Dashboard | logo; goes to the Dashboard |
| `rail-account-<pubkey>` | Tab | account name, or "New account" | one per stored account; `Value: on` for the active one |
| `rail-add-account` | Button | Add account | the "+"; opens account setup |
| `rail-relay` | Label | Relay connected / Relay disconnected | screen shows "relay" and a coloured dot; the label names the colour's meaning |
| `rail-accounts` | Tab | Accounts | on for the Accounts and account pages |
| `rail-settings` | Tab | Settings | |
| `sidebar-account` | Button | account name, or "View only" | account chip; opens the account's Trust tab |
| `sidebar-account-name` | Label | account name, or "View only" | |
| `sidebar-account-npub` | Label | short npub, or "Add one with the + button" | |
| `sidebar-account-address` | Label | payout Lightning address | only when set |
| `sidebar-remove-account` | Button | Remove this account / Click again to remove | first press arms, second removes |
| `sidebar-remove-warning` | Label | sats-left warning | only while armed with sats left |
| `sidebar-account-step` | Button | Next account | the "›" chevron |
| `nav-<screen>` | Tab | screen name | `dashboard`, `bounties`, `payments`, `claimants`, `leaderboards`, `wallet`, `tags`, `chat`; `nav-bounties` has the open count as description when it is above 0 |
| `sidebar-open-title`, `sidebar-promised-title`, `sidebar-paid-title` | Label | OPEN BOUNTIES, PROMISED, PAID SO FAR | |
| `sidebar-open`, `sidebar-promised`, `sidebar-paid` | Label | the figure | "…" while loading, "—" on failure; the "sats" unit is not in the label |
| `sidebar-open-note`, `sidebar-promised-note`, `sidebar-paid-note` | Label | the note under the figure | |
| `top-search` | TextInput | Search | |
| `top-auto-pay` | Label | Auto-pay armed / Auto-pay off | only while a bounty is focused |

Nav and placeholder ids derive from the screen names; renaming a screen
renames its ids.

## Placeholder screens and the Wallet screen

| id | role | label | meaning |
|---|---|---|---|
| `placeholder-<screen>-title` | Heading | screen name | Payments, Claimants, Leaderboards, Tags, Settings |
| `placeholder-<screen>` | Label | Coming soon. | |
| `wallet-title` | Heading | Wallet: <name> / Wallet | |
| `wallet-no-account` | Label | No account — press + in the rail to add one. | |

## Account setup

| id | role | label | meaning |
|---|---|---|---|
| `setup-title-choose` | Heading | Your Nostr account | choose card |
| `setup-create` | Button | Create a new account | |
| `setup-paste` | Button | I already have a key | |
| `setup-title-create` | Heading | Create a new account | create card |
| `setup-name-label` | Label | Name (optional) | |
| `setup-name` | TextInput | Name (optional) | |
| `setup-wallet-note` | Label | the Coinos note | |
| `setup-create-go` | Button | Create — Enter | |
| `setup-title-paste` | Heading | Use your Nostr key | paste card |
| `setup-key` | TextInput | Secret key | masked; never the key text |
| `setup-paste-hint` | Label | Enter continues | |
| `setup-error` | Label | the error | create and paste cards |
| `setup-back` | Button | ← Back | |
| `setup-working` | Label | the spinner text | e.g. "Loading the account…" |
| `setup-title-browse` | Heading | Just look around | |
| `setup-browse` | Button | Continue without an account — esc | first launch only |
| `setup-cancel` | Button | Cancel — esc | when adding a further account |
| `setup-step-identity-title` | Heading | Your Nostr identity | ready, step 1 |
| `setup-step-identity-tag` | Label | saved on this computer | |
| `setup-npub` | Label | short npub | |
| `setup-copy-npub` | Button | Copy npub | |
| `setup-identity-warning` | Label | no-profile warning | pasted key with no profile |
| `setup-step-wallet-title` | Heading | A Lightning wallet | ready, step 2 |
| `setup-step-wallet-tag` | Label | Coinos wallet ready — <address> / Lightning address: <address> | |
| `setup-wallet-warning` | Label | profile-check warning | |
| `setup-step-community-title` | Heading | Your Trusted Community | step 3 while its card loads |

The ready steps also embed the wallet panel (`wallet-*`) and the community
card (`community-*`). `community-dashboard` leaves setup.

## Dashboard

`<row>` is `dash-attention-<kind>` or, for a row about one account,
`dash-attention-<pubkey>-<kind>`.

| id | role | label | meaning |
|---|---|---|---|
| `dash-heading` | Heading | Dashboard | |
| `dash-summary` | Label | N account(s) · watching <host> | |
| `dash-new-item` | Button | + New DList + bounty / + Submit a bounty claim | depends on whether the active account is the issuer |
| `dash-attention-heading` | Heading | Needs attention | |
| `dash-attention-count` | Label | the count | only with rows |
| `dash-attention-list` | List | Needs attention | |
| `<row>` | Group | the row title; description = its description | kinds without an account: `read-only`, `relay-disconnected`, `bounty-list-unavailable`; per account: `map-missing`, `map-deactivated`, `map-no-rank-row`, `map-unreadable`, `no-follows`, `no-verified-follower` |
| `<row>-title`, `<row>-description` | Label | visible title, description | error text and relay URL are in the description |
| `<row>-action` | Button | Fix → / Retry → | only on rows with an action |
| `dash-attention-clear` | Label | Nothing needs you right now. The carpet flies itself. / Loading bounties… | |
| `dash-activity-heading` | Heading | Recent activity | |
| `dash-activity-list` | List | Recent activity | |
| `dash-activity-<at>-<hash>` | Group | the event text | `<at>` unix seconds, `<hash>` FNV-1a of the text, `-2`, `-3` for repeats; find rows by label, not by computing the id |
| `<activity row>-meta` | Label | HH:MM:SS UTC · <relative> | |
| `dash-activity-empty` | Label | Nothing yet. New claims and payments show up here. | |

The map row's kind changes with the verdict, so match the prefix
`dash-attention-<pubkey>-map-`.

## Bounties

`<row>` is `bounty-row-<bounty-id>`; `<claim>` is `claim-<event-id>`.

| id | role | label | meaning |
|---|---|---|---|
| `bounties-title` | Heading | Bounties | |
| `bounties-keys-hint` | Label | the key hint line | |
| `bounties-new` | Button | + New DList + bounty (⌘N) / + Submit a bounty claim (⌘N) | |
| `bounties-tabs` | TabList | Bounty lists | |
| `bounties-tab-house` | Tab | House bounties (N) | count once loaded |
| `bounties-tab-eligible` | Tab | Eligible for me to claim (N) | only with an account |
| `bounties-list` | List | Bounties | |
| `bounties-loading` | Label | Loading bounties… | |
| `bounties-empty` | Label | the empty text of the open tab | |
| `bounties-eligible-identity` | Button | the share-your-npub hint | Eligible tab, empty |
| `bounties-error` | Label | Bounties didn't load: … / Eligible bounties didn't load: … | |
| `bounties-retry` | Button | Retry (⌘R) | |
| `<row>` | ListItem | list name (d-tag) | `(selected)` when selected; press to select |
| `<row>-status`, `<row>-sats`, `<row>-paid`, `<row>-auto-pay`, `<row>-qualify` | Label | the pills of the row | folded into the row's line by macos-harness |
| `bounty-detail-empty` | Label | Select a bounty (↑/↓). | |
| `bounty-detail-loading` | Label | Loading bounty… | |
| `bounty-detail-title` | Heading | list name | |
| `bounty-detail-status`, `bounty-detail-paymode`, `bounty-detail-qualify` | Label | the pills | |
| `bounty-detail-criteria` | Label | criteria | |
| `bounty-detail-reward`, `bounty-detail-cap` | Label | N sats (/ item) | |
| `bounty-detail-min-rank` | Label | the rank | |
| `bounty-detail-slots` | Label | x paid · y open | |
| `bounty-detail-new-on-list` | Button | New bounty on this list | |
| `bounty-detail-claims-heading` | Heading | Claims | |
| `claim-list` | List | Claims | |
| `claim-list-loading` | Label | Loading claims… / Created — syncing with the instance… | |
| `claim-list-empty` | Label | No claims to show. … | |
| `<claim>` | Group | item name | one per claim |
| `<claim>-item`, `<claim>-author` | Label | item name, by <short pubkey> | |
| `<claim>-submitted`, `<claim>-attempted`, `<claim>-blocked`, `<claim>-receipt` | Label | timeline rows | |
| `<claim>-payment` | Label | payment state: Auto-pay attempting…, Paid — waiting for the zap receipt, Payment settled, Paid, no receipt (…), Auto-pay failed (…), Payment status: …, No auto-payment row yet | same id whatever the state |
| `<claim>-submitted-meta`, `-attempted-meta`, `-payment-meta` | Label | HH:MM:SS UTC · <relative> (· N sats) | `-payment-meta` carries the paid amount |
| `<claim>-receipt-meta` | Label | N sats · Ns after the claim | |

### New bounty and claim forms

| id | role | label | meaning |
|---|---|---|---|
| `bounty-form` | Group | New DList + bounty / New bounty on this list | |
| `bounty-form-title` | Heading | same | |
| `bounty-form-cancel` | Button | Cancel (esc) | |
| `bounty-form-error` | Label | the error | |
| `bounty-form-status` | Label | the hint, Checking the list… or Publishing… | |
| `bounty-form-list-label`, `bounty-form-list` | Label | List, the list name | adding to an existing list |
| `bounty-form-item-singular`, `-item-plural`, `-description`, `-criteria`, `-amount`, `-cap`, `-min-rank` | TextInput | the field's caption | each has a `<field>-label` caption node |
| `claim-form` | Group | Claim an item on <list> · N sats per item | |
| `claim-form-title`, `claim-form-cancel`, `claim-form-error`, `claim-form-status` | as above | | |
| `claim-form-item-name` | TextInput | Item name | with `claim-form-item-name-label` |
| `claim-form-note` | Label | the payout note | |

## Wallet panel

On the Wallet screen and in the account-setup ready steps (never both).

| id | role | label | meaning |
|---|---|---|---|
| `wallet-address-label`, `wallet-address` | Label | Lightning address, the address | |
| `wallet-qr` | Image | Funding QR code | Label "Could not draw the QR code." when it can't be drawn |
| `wallet-qr-hint` | Label | the scan hint | |
| `wallet-balance` | Label | Balance: N sats / Balance: … / Received N sats / Waiting for sats… | |
| `wallet-balance-delta` | Label | +15 was 40 / −30 was 439 | fades out after a change; uses U+2212 |
| `wallet-send-to` | TextInput | Send to a Lightning address | names the field; placeholder is "name@domain.com" |
| `wallet-send-sats` | TextInput | Amount in sats | placeholder is "sats" |
| `wallet-send` | Button | Send | inert while sending |
| `wallet-send-status` | Label | Sending… / Sent N sats to <address> | |
| `wallet-send-error` | Label | the send error | same slot as the status, separate id |
| `wallet-username` | Label | Coinos username: … | after a signup in this session |
| `wallet-copy-username`, `wallet-copy-password` | Button | Copy username, Copy password | |
| `wallet-other-address` | Label | Your profile pays X, not this wallet. … | |
| `wallet-publish-warning`, `wallet-publish-error` | Label | | |
| `wallet-retry-publish` | Button | Retry publishing | |
| `wallet-opening` | Label | Opening a Coinos wallet… | |
| `wallet-managed-elsewhere` | Label | The house key's wallet is not managed here. | |
| `wallet-checking` | Label | Checking your profile… | |
| `wallet-profile-error` | Label | Couldn't read this profile. … | |
| `wallet-profile-retry` | Button | Try again | |
| `wallet-no-address` | Label | No Lightning address yet — … | |
| `wallet-create` | Button | Create a Coinos wallet for this account | |
| `wallet-error` | Label | the panel error | |

## Chat

| id | role | label | meaning |
|---|---|---|---|
| `chat-transcript` | List | Conversation | description "Claude is replying" while a reply streams |
| `chat-message-<n>` | ListItem | the bubble's visible text; "thinking…" while empty | description `user` or `assistant`; `<n>` is the position from 0 |
| `chat-composer` | Group | Message Claude… | its child is the text entry area |

Messages have no data id and only append, so the position is stable. With no
API key at launch the notice is message 0. An error replaces the reply's text
on the same id; its label starts "Something went wrong." or "Claude sent an
empty reply.". There is no send button: Enter sends.

## Accounts

`<card>` is `accounts-card-<pubkey>`.

| id | role | label | meaning |
|---|---|---|---|
| `accounts-title` | Heading | Accounts | |
| `accounts-add` | Button | + Add an account | |
| `accounts-list` | List | Accounts | |
| `<card>` | Group | account name | |
| `<card>-name`, `-active`, `-npub`, `-payout`, `-balance` | Label | | `-active` only on the active account; `-balance` is sats or "—" |
| `<card>-switch`, `<card>-manage` | Button | Switch, Manage | |

## Account page

| id | role | label | meaning |
|---|---|---|---|
| `account-back` | Button | ← All accounts | |
| `account-title` | Heading | account name | |
| `account-active` | Label | active | |
| `account-npub` | Label | short npub | |
| `account-view-profile` | Link | View profile | |
| `account-switch` | Button | Switch to this account | |
| `account-tabs` | TabList | Account sections | |
| `account-tab-identity`, `-trust`, `-progress`, `-wallet`, `-limits` | Tab | Identity & keys, Trust, Progress, Wallet, Limits & defaults | |
| `account-progress-soon`, `account-wallet-soon`, `account-limits-soon` | Label | Coming soon. | |
| `account-none` | Label | No account selected. | |
| `account-removed` | Label | This account was removed. | after a remove |

### Identity & keys

| id | role | label | meaning |
|---|---|---|---|
| `account-public-title` | Heading | Public identity | |
| `account-npub-text` | Label | full npub | |
| `account-copy-npub` | Button | copy / copied | |
| `account-identity-view-profile` | Link | View this profile on Brainstorm ↗ | |
| `account-secret-title` | Heading | Secret key (nsec) | |
| `account-nsec-text` | Label | masked dots / Secret key | never the key |
| `account-reveal-nsec`, `account-hide-nsec` | Button | Reveal, Hide | |
| `account-copy-nsec` | Button | copy / copied | |
| `account-nsec-warning` | Label | the warning, ending "It hides again in Ns." | while revealed |
| `account-remove-title` | Heading | Remove this account | |
| `account-remove` | Button | Remove… / Click again to remove | below the fold: scroll first |
| `account-remove-warning` | Label | Checking balance… / the sats warning | while armed |

### Trust

`<k>` is `own` or `instance`; `<key>` is `map`, `own` or `instance`.

| id | role | label | meaning |
|---|---|---|---|
| `account-trust-map-title` | Heading | Treasure Map | |
| `account-trust-map-status` | Label | Located / Not located / Checking | |
| `account-trust-map-published`, `-found-on`, `-entries` | Label | | when located |
| `account-trust-map-rank-provider`, `-provider-relay`, `-brainstorm-key` | Label | | with a rank row |
| `account-trust-map-copy-rank-provider` | Button | copy / copied | |
| `account-trust-map-deactivated`, `-checking`, `-error` | Label | | |
| `account-trust-map-check-again` | Button | Check again | |
| `account-trust-missing-map` | Label | the missing-map sentence | |
| `account-trust-brainstorm-link` | Link | Open Brainstorm ↗ | |
| `account-trust-raw-<key>` | Button | ▸ View raw event / ▾ Hide raw event (`map`); ▸ View most recent event / ▾ Hide most recent event (`own`, `instance`) | |
| `account-trust-assertions-title` | Heading | Trusted Assertions | |
| `account-trust-assertions-status` | Label | Located / None found / Couldn’t check / Checking | |
| `account-trust-own-title`, `account-trust-instance-title` | Heading | group titles | |
| `account-trust-instance-provider` | Label | Scores seen by … | |
| `account-trust-<k>-located`, `-recent`, `-rank`, `-hops`, `-none`, `-checking`, `-error` | Label | | |
| `account-trust-own-pov-note` | Label | | |
| `account-trust-<k>-check-again` | Button | Check again | |
| `account-trust-min-rank-title`, `account-trust-min-rank` | Heading, Label | Minimum rank, the number | |
| `account-trust-determination-title` | Heading | Trust Determination | |
| `account-trust-pov-name`, `-pov-npub`, `-scoring-method` | Label | | |
| `account-trust-wot-title` | Heading | Web of trust | |
| `account-trust-following`, `account-trust-followers` | Label | the count or "—" | with `-label` and `-note` nodes |
| `account-trust-wot-brainstorm` | Link | See the full graph on Brainstorm ↗ | |
| `account-trust-recalc` | Button | Recalculate | the Trust tab's recalc block; see `{rc}` below |

The Trust tab also shows the community steps below when the map is missing.

## Community (trusted-community setup)

On the Trust tab and in the setup ready steps. `{rc}` is the recalc prefix:
`community-recalc` inside step 1, `account-trust-recalc` on the Trust tab.

| id | role | label | meaning |
|---|---|---|---|
| `community-title` | Heading | Your Trusted Community | setup card |
| `community-checking`, `community-read-failed` | Label | | |
| `community-check-again` | Button | Check again | |
| `community-note-calculated`, `community-note-missing`, `community-note-info` | Label | the note text | |
| `community-dashboard` | Button | Take me to the Magic Carpet Dashboard / …, I'll visit Brainstorm.world later | leaves setup; the longer label when no map was found |
| `community-loading` | Label | Loading setup steps… | |
| `community-step-follow`, `-calculate`, `-publish-map` | Heading | the step title | |
| `community-step-<step>-state` | Label | Step N / Done | screen shows the number or ✓ |
| `community-owner-<pubkey>` | Group | <name> <npub> | the recommended account |
| `community-owner-<pubkey>-name`, `-badge`, `-npub` | Label | | |
| `community-owner-<pubkey>-follow` | Button | Follow / Following… / ✓ Following | |
| `community-already-follow` | Label | You already follow N account(s). … | |
| `community-search` | TextInput | Search profiles to follow… | |
| `community-search-status` | Label | Searching… / No profiles match “q”. … / Search is down. … / That's a private key. Paste an npub. | |
| `community-hits` | List | Search results | |
| `community-hit-<pubkey>` | Group | <name> <nip05> | one per result |
| `community-hit-<pubkey>-nip05` | Label | nip05 | only when the profile has one |
| `community-hit-<pubkey>-follow` | Button | Follow / Following… | spinner-only while following |
| `community-followed-<pubkey>` | ListItem | name | the green chips |
| `community-follow-saved` | Label | Saved on N of M Nostr server(s) / Already following. Nothing to save. | |
| `community-show-relays` | Button | · show servers / · hide servers | |
| `community-follow-saved-hosts`, `community-follow-refused-hosts` | Label | | after show servers |
| `community-follow-error` | Label | | |
| `community-first-list-note` | Label | | confirm box |
| `community-first-list`, `community-first-list-cancel` | Button | Start a new follow list, Cancel | |
| `{rc}` | Button | Recalculate | `unavailable` when inactive |
| `{rc}-follow-first`, `{rc}-status`, `{rc}-last-run` | Label | | run status: sending, queue position, Calculating your scores…, Publishing…, Done. …, or a failure |
| `community-calc`, `community-calc-retry` | Button | Calculate my trust scores | `unavailable` when inactive |
| `community-calc-hint`, `community-calc-status` | Label | | |
| `community-map` | Button | Publish my Treasure Map | `unavailable` when inactive |
| `community-map-hint`, `community-map-status`, `community-map-error` | Label | | |
| `community-check-setup` | Button | Try again | |
| `community-skip` | Label | the skip line | its brainstorm.world link is not a separate node |

## Not in the tree

- Toasts: gpui-component's Notification sets no role.
- The window title bar text and the logo art.
- Static descriptions under card titles built by `dashboard::muted`.
- The Bounties claim-count badge and the nav count badge (the count is in
  `nav-bounties`' description and `sidebar-open`).
- The brainstorm.world link inside `community-skip`: gpui's InteractiveText
  reports one Label for the whole line.
