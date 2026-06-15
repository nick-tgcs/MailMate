# MailMate Thunderbird extension

Thin MailExtension (MV3, Thunderbird ESR 140+) adapter for MailMate. The extension is a
**thin adapter**: all decisions live in the Rust native host (`mailmate-native-host`);
the extension only relays events and applies host-returned safe actions.

## Files

- `manifest.json` — MV3 manifest; permissions for native messaging, menus, message
  read/move/update, accounts, and compose; background event page.
- `native.js` — host name, the `ping` envelope, and `NativeHost`: a long-lived port with
  request/response correlation, an unsolicited-notification fan-out (the host pushes
  `classification_ready` / `mail_command` at any time), **and the single source-of-truth
  `HostStatus`** — on connect it runs the `hello` handshake (host/protocol version,
  capabilities, the secret-free drafting/retention posture), heartbeats with `ping`, and
  exposes `onStatusChange` + `reconnect()`.
- `action.html` / `action.css` / `action.js` — the toolbar button popup: the connection
  mini-hub / **recovery card**. Reads `HostStatus` from the background, renders
  connected / connecting / offline / version-mismatch, and offers a one-click Retry when the
  host is down. (The popup owns no port — it drives the host through the background.)
- `panel.html` / `panel.css` / `panel.js` — the **per-message header panel**
  (`messageDisplayAction` popup): the reading + correcting surface. Resolves the displayed
  message, asks the background to `classify_message` it, and renders the verdict (category +
  a *banded* confidence + `why`), the `apply_state`-partitioned action blocks
  (auto-applied · suggested · blocked), and the three one-click corrections. Owns no port —
  it drives the host through the background's `mm:*` router.
- `message_reader.js` — read a Thunderbird message into the host's `classify_message` /
  `new_mail` wire shape (headers always; body only when retention allows; remote content
  never loaded for classification).
- `context_menu.js` — register the message context-menu actions (classify, draft reply,
  mark spam / not spam) and dispatch each to the host.
- `drafts.js` — open review-required draft replies (`compose.beginReply` →
  `compose.saveMessage({mode:'draft'})`, **never sent**) and execute the host's
  `mail_command` safe actions (tag/move/junk/read/flag), reporting each result back.
- `dashboard.html` / `dashboard.css` / `dashboard.js` — the **dashboard space** (the product's
  home): a first-class Thunderbird `spaces` tab rendering a small SPA with **Review**,
  **Follow-ups**, **Proposals**, and **Activity** tabs, the connection banner, the first-run
  onboarding walkthrough, empty states, and the `›` deep-link grammar (focus a message in the
  Mail space). Review is built from buffered `classification_ready` decisions; Activity from
  `list_recent_activity`; Proposals (read-only here) from `list_pending_reviews`. Owns no port —
  drives the host through the background's `mm:*` router.
- `icons/mailmate.svg` — the spaces-toolbar button icon (fixed mid-tone fills legible on both
  light and dark themes, with the brand-blue spark; a space icon is an image resource, so it does
  not theme-tint).
- `background.js` — the event page and **single native-port owner**:
  `messages.onNewMailReceived` intake (registered synchronously), notification routing,
  context menus, `messages.onMoved` filing capture, the `HostStatus` → toolbar-badge wiring,
  the **dashboard space registration + aggregate badge** (`review + needs-attention follow-ups +
  pending proposals`) and the `storage.session` review-queue buffer, and the popup/dashboard ↔
  host message router (`mm:getStatus` / `mm:reconnect` for the recovery card; `mm:classify` /
  `mm:apply` / `mm:dismiss` / `mm:undo` / `mm:correctLabel` / `mm:notJunk` / `mm:move` /
  `mm:folders` for the panel; `mm:reviewQueue` / `mm:resolveReview` / `mm:listActivity` /
  `mm:listProposals` / `mm:settings` / `mm:setPause` for the dashboard).

## The Phase-10 capabilities

| # | Capability | Wire |
|---|---|---|
| 1 | Read selected message | `classify_message` request |
| 2 | Listen to new mail | `messages.onNewMailReceived` → `new_mail` → `classification_ready` push |
| 3 | Context-menu actions | `menus` → the matching request |
| 4 | Open draft replies | `draft_reply` response → `compose` draft (review-required) |
| 5 | Apply safe actions | `classification_ready` / `mail_command` → `drafts.js` |
| 6 | Record actions & results | `record_user_action` (corrections → feedback; results → audit) |

## Milestone 2 UX — the dashboard space

The `spaces` dashboard tab (`dashboard.*`) is MailMate's home and the single source of truth for
"what needs me." The host addition it consumes is `list_recent_activity` (the Activity tab's
cross-message global stream + the six event-type filter families); it otherwise reuses the M1
wires (`classification_ready` buffering for Review, `list_pending_reviews` for Proposals counts,
`explain_decision`, the panel's `mm:apply` / `mm:dismiss` / `mm:undo` for per-row actions).

| Surface | Wire | Status |
|---|---|---|
| Dashboard shell + Review queue | buffered `classification_ready` (in `storage.session`); per-row Approve/Dismiss/Undo via the panel's `mm:*` handlers | **shipped** (`dashboard.*`) |
| Activity / Explain timeline | `list_recent_activity` (global stream + filter families) | **shipped** |
| Onboarding walkthrough + connection banner + empty states | `storage.local` `mm:onboarded`; `HostStatus` | **shipped** |
| Aggregate space badge | `review + needs-attention follow-ups + pending proposals`, via `spaces.update` | **shipped** |

Honest M2 boundaries: the **Proposals** tab is read-only (approve/reject/edit + conflict review
land with `review_rule_proposal` in Milestone 3); the **Follow-ups** tab is a placeholder until
`list_followups` (Milestone 4); **Pause** and **Settings** call their real host endpoints
(`set_pause`, `options_ui`) and degrade with an explicit message until those land in Milestone 4;
the Review buffer is volatile across a browser restart (the durable `list_review_queue` reopen is
the documented future addition).

## Milestone 1 UX (shipped)

The Thunderbird UX from [`../interaction-design.md`](../interaction-design.md). The host
protocol it needs (`hello`, the per-action `apply_state` field, the `classification_corrected`
/ `action_undone` / `suggestion_dismissed` discriminants) is live in the host.

| Surface | Wire | Status |
|---|---|---|
| Connection health (`HostStatus`, toolbar badge, recovery card) | `hello` handshake + `ping` heartbeat; popup ↔ background `mm:getStatus` / `mm:reconnect` | **shipped** (`native.js`, `background.js`, `action.*`) |
| Per-message panel (`messageDisplayAction`) | `classify_message` + `apply_state` partition; corrections via `classification_corrected` / `junk_changed` / `onMoved` | **shipped** (`panel.*`, `background.js` `mm:*` router) |

Two honest M1 boundaries on the panel: the confidence is a client-side *band* (a calibrated
numeric band is a host addition), and the `auto_applied` block is render-complete but unseen
until a crystallized rule exists (Milestone 2) — a manual classify applies nothing, so M1's
live loop is verdict → suggestion → one-click correction.

## Wire contract & testing

Native messaging frames are a 32-bit native-byte-order length prefix + UTF-8 JSON. The
exact frames these scripts produce are validated by the host's end-to-end harness test,
which replays them byte-for-byte and asserts the host's responses:

```
cargo test -p mailmate-native-host --test extension_harness
```

This is the e2e substitute the Testing Strategy permits when true Thunderbird automation
is unavailable in CI.

## Registering the native host (manual, for local dev)

The host prints its manifest:

```
cargo run -p mailmate-native-host -- manifest
```

Write that JSON to the per-OS native-messaging location under the name
`com.mailmate.host` (a dedicated `install` subcommand automates this in a later phase).

## Wiring status

The extension speaks the full Phase-10 protocol. On the host side, the `HostRouter` that
serves these requests is tested against in-memory fakes; the production composition root
that injects the real classification cascade, provider, and storage into the router is
deferred to Phase 12 (provider/retention settings + config), so the shipped binary still
runs the protocol/ping loop until then.
