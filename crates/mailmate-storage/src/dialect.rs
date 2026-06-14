//! The ONE place engine-specific SQL fragments live.
//!
//! For the Phase-2 schema — append-only, `TEXT`/`INTEGER` columns, app-generated
//! prefixed-string PKs — divergence is near-zero, so SQLite is the only implemented
//! dialect and the migration overlay carries the only per-engine SQL today. This module
//! holds the small fragment helpers a second engine will need (column types, overlay
//! labels), so a server backend slots in here rather than scattering SQL through the
//! repositories.

use mailmate_ports::storage::Dialect;

/// The column type for an opaque `*_json` value in `dialect`: `TEXT` on SQLite, `jsonb`
/// on Postgres, `JSON` on MySQL. The portable default treats these columns as opaque and
/// does structured work in Rust, so the *type* is the only per-engine part today.
#[must_use]
pub fn json_column_type(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Sqlite => "TEXT",
        Dialect::Postgres => "jsonb",
        Dialect::MySql => "JSON",
    }
}

/// The boolean literal for `dialect`. SQLite/MySQL use `INTEGER` `1`/`0`; a Postgres
/// overlay that opts into a real `BOOLEAN` column would use `TRUE`/`FALSE`.
#[must_use]
pub fn bool_literal(dialect: Dialect, value: bool) -> &'static str {
    match (dialect, value) {
        (Dialect::Postgres, true) => "TRUE",
        (Dialect::Postgres, false) => "FALSE",
        (_, true) => "1",
        (_, false) => "0",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_column_types_map_per_dialect() {
        assert_eq!(json_column_type(Dialect::Sqlite), "TEXT");
        assert_eq!(json_column_type(Dialect::Postgres), "jsonb");
        assert_eq!(json_column_type(Dialect::MySql), "JSON");
    }

    #[test]
    fn bool_literals_map_per_dialect() {
        assert_eq!(bool_literal(Dialect::Sqlite, true), "1");
        assert_eq!(bool_literal(Dialect::Sqlite, false), "0");
        assert_eq!(bool_literal(Dialect::Postgres, true), "TRUE");
        assert_eq!(bool_literal(Dialect::Postgres, false), "FALSE");
        assert_eq!(bool_literal(Dialect::MySql, true), "1");
    }
}
