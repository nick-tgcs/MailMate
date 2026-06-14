# MailMate Thunderbird extension

Thin MailExtension (MV3, Thunderbird ESR 140+) adapter for MailMate. The extension is a
**thin adapter**: all decisions live in the Rust native host (`mailmate-native-host`);
the extension only relays events and applies host-returned safe actions.

## Phase 1 (this skeleton)

- `manifest.json` — MV3 manifest, `nativeMessaging` permission, background event page.
- `native.js` — host name + envelope helpers (the wire contract).
- `background.js` — opens the native port and sends one `ping` at startup, logging the
  `pong`.

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

## Later phases

`message_reader.js`, `context_menu.js`, and `drafts.js` (Phase 10) add: read selected
message, new-mail intake (`messages.onNewMailReceived`), context-menu actions, draft
replies (`compose.beginReply` → `compose.saveMessage` as a review-required draft — never
auto-sent), and applying host-returned safe actions.
