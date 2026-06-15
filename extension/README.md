# MailMate Thunderbird extension

Thin MailExtension (MV3, Thunderbird ESR 140+) adapter for MailMate. The extension is a
**thin adapter**: all decisions live in the Rust native host (`mailmate-native-host`);
the extension only relays events and applies host-returned safe actions.

## Files

- `manifest.json` — MV3 manifest; permissions for native messaging, menus, message
  read/move/update, accounts, and compose; background event page.
- `native.js` — host name, the `ping` envelope, and `NativeHost`: a long-lived port with
  request/response correlation **and** an unsolicited-notification fan-out (the host pushes
  `classification_ready` / `mail_command` at any time).
- `message_reader.js` — read a Thunderbird message into the host's `classify_message` /
  `new_mail` wire shape (headers always; body only when retention allows; remote content
  never loaded for classification).
- `context_menu.js` — register the message context-menu actions (classify, draft reply,
  mark spam / not spam) and dispatch each to the host.
- `drafts.js` — open review-required draft replies (`compose.beginReply` →
  `compose.saveMessage({mode:'draft'})`, **never sent**) and execute the host's
  `mail_command` safe actions (tag/move/junk/read/flag), reporting each result back.
- `background.js` — the event page: `messages.onNewMailReceived` intake (registered
  synchronously), notification routing, context menus, `messages.onMoved` filing capture,
  and the startup `ping`.

## The six Phase-10 capabilities

| # | Capability | Wire |
|---|---|---|
| 1 | Read selected message | `classify_message` request |
| 2 | Listen to new mail | `messages.onNewMailReceived` → `new_mail` → `classification_ready` push |
| 3 | Context-menu actions | `menus` → the matching request |
| 4 | Open draft replies | `draft_reply` response → `compose` draft (review-required) |
| 5 | Apply safe actions | `classification_ready` / `mail_command` → `drafts.js` |
| 6 | Record actions & results | `record_user_action` (corrections → feedback; results → audit) |

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
