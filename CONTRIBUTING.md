# Contributing to MailMate

MailMate is **test-driven by mandate**. No production behaviour is written before a
failing automated test exists for it and has been run to prove it fails for the
expected reason. This is not a style preference — it is enforced by CI and is the
condition for a change being mergeable.

## The loop (RED → GREEN → REFACTOR)

1. **RED** — write a failing test for the behaviour.
2. **Verify RED** — run the specific test; confirm it fails for the *expected* reason.
3. **GREEN** — write the smallest production change that makes it pass.
4. **Verify GREEN** — run the test and its group.
5. **REFACTOR** — clean up only once green.
6. **Regression** — run the full applicable suite before pushing.

Every feature area carries all three test layers — **unit**, **integration**, and
**end-to-end / harness** — unless a documented technical limitation prevents one and an
approved substitute is added (see *Testing Strategy* in [architecture.md](architecture.md)).

## Gates (run before AND after every change)

One command runs the same gates CI enforces:

```sh
ci/scripts/test-all.sh
```

It runs, in order:

| Gate | Command |
|---|---|
| Formatting | `cargo fmt --all --check` |
| Lint (warnings denied) | `cargo clippy --workspace --all-targets -- -D warnings` |
| Build | `cargo build --workspace --all-targets` |
| Tests | `cargo test --workspace` |
| No-backend-leakage (source) | `ci/scripts/test-interface-no-backend-leakage.sh` |
| Dependency hygiene (exact pins) | `ci/scripts/test-real-adapter-deps.sh` |
| Coverage ≥ 80% | `ci/scripts/coverage-gate.sh` |

The **architecture law** — `mailmate-core` depends only on `mailmate-ports` +
`mailmate-common`, never on a backend (storage engine, AI runtime, ML engine) — is
enforced two ways: a `cargo metadata` test in `mailmate-arch-test` (authoritative,
catches direct/transitive/optional/dev edges) and the complementary source scan above.

## Coverage

Workspace **total line coverage must stay ≥ 80%** before a phase advances. Compile-time
constructs (object-safety proofs, derive glue) legitimately depress per-file numbers;
the gate measures the workspace total, which is the honest figure.

## Branches & commits

- `develop` is the integration branch; `main` is the release branch. Feature work uses
  `feat/…`, `fix/…`, `docs/…`, `test/…`, `ci/…`, `refactor/…`, `chore/…`.
- Open PRs into `develop`; the `validation-gate` check must pass.
- Exact-pin every third-party dependency (`= x.y.z`) in the root manifest's
  `[workspace.dependencies]`.

## Where the rules live

[architecture.md](architecture.md) is the single source of truth: testing policy, CI
policy, the native-messaging protocol, the rule/policy/learning design, the storage
seam, and the safety invariants (no auto-send, no auto-delete, review-required drafts).
