<!--
MailMate is TDD-mandated: every production change starts from a failing test.
Fill in every section; "n/a" is acceptable where genuinely not applicable.
-->

## Summary

<!-- What changed and why. -->

## TDD

- **Failing test observed first (RED):** <!-- name the test + the failure it showed -->
- **Unit tests added/updated:**
- **Integration tests added/updated:**
- **End-to-end / harness tests added/updated:**

## Impact checklist

- [ ] **Privacy** — no new default body retention or provider egress (or: described below).
- [ ] **Policy guard** — no new auto-send/auto-delete path; `RequireReview` preserved where required.
- [ ] **Migration** — schema change is additive + has fresh + upgrade migration tests (or: n/a).
- [ ] **Provider** — provider-specific types stay inside the provider adapter (or: n/a).
- [ ] **LoRA / training-data** — capture/redaction/split/export tested; no GPU/external trainer required (or: n/a).
- [ ] **Coverage** — workspace total line coverage stays ≥ 80% (`ci/scripts/coverage-gate.sh`).
- [ ] **Architecture** — `mailmate-core` still depends only on ports + common (arch guards green).

## Notes

<!-- Anything reviewers should know: trade-offs, follow-ups, open questions. -->
