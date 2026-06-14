# PostgreSQL migration overlay (opt-in server engine — documented stub)

This directory is the per-dialect overlay for the opt-in PostgreSQL backend. It exists
from day one so the `common` + per-dialect overlay structure is real, but it is a
**documented stub**: no `.sql` files ship here yet, and no PostgreSQL backend is built.

When the server engine is implemented (see *Storage Strategy* in `architecture.md`), each
`NNNN_*.sql` here mirrors the matching `common/NNNN_*.sql`, supplying only the diverging
fragments: `jsonb` columns instead of `TEXT` for `*_json`, server-side view bodies,
`BOOLEAN`/`TIMESTAMPTZ` types if the overlay opts into them, and any `RETURNING`/upsert
SQL a future non-append-only table needs. SQLite remains the only required, always-on
engine; PostgreSQL stays second-class (opt-in, non-blocking CI legs) until a real need.
