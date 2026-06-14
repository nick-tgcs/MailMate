# MailMate

A local-first, Rust-native mail assistant for Thunderbird. It learns explicit,
versioned, auditable, **reversible** rules from how you actually file, tag, and reply —
and crystallises each learned trait into a **model-free deterministic rule**, so once a
behaviour is learned it runs without any AI/LLM in the loop. Models are a teacher and a
fallback, never the executor of a learned trait.

Safety is structural, not advisory: a dedicated **policy guard** forbids auto-delete and
auto-send, drafts are always `CreateDraft + RequireReview`, and nothing bypasses rules,
validation, or human review. The system is fully functional with **zero AI providers
configured**; a local provider unlocks generative features with **zero egress**.

## Architecture in one breath

Hexagonal / ports-and-adapters. `mailmate-core` holds the product and depends only on
**ports** + **common value types** — never on a concrete backend. Every backend (mail
client, storage engine, AI provider, ML trainer, Tier-2 classifier, clock, secrets) is a
config-selected adapter behind a trait, with a deterministic fake for tests. The
on-device learned layer rides on **Burn** (Tier-2 classifier + trainer), behind those
ports; the generative foundation model runs **frozen** on a mature engine
(Ollama / llama.cpp). This mirrors the sibling project
[idiolect](https://github.com/nick-tgcs/idiolect).

The full design — data model, native-messaging protocol, rule/policy/learning system,
storage strategy, testing and CI policy, and the phased build order — lives in
**[architecture.md](architecture.md)**.

## Build & test

```sh
ci/scripts/test-all.sh   # fmt + clippy + build + tests + arch guards + coverage gate
```

See **[CONTRIBUTING.md](CONTRIBUTING.md)** for the TDD mandate and the gate details.

## Status

Built phase by phase under strict TDD (RED → GREEN → REFACTOR) with a ≥ 80% coverage
gate per phase. See the *Implementation Sequence* in [architecture.md](architecture.md).

## License

AGPL-3.0-only.
