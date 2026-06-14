//! No-backend-leakage guard, enforced from OUTSIDE the hexagon.
//!
//! Two layers, both required (per the hexagonal review): pure check logic
//! ([`check_core_isolation`], unit-tested with synthetic inputs incl. failing cases)
//! plus a `cargo metadata` extractor ([`extract_core`], exercised against the real
//! workspace by the integration test). The crate deliberately does NOT path-depend on
//! core/ports/common, so inspecting them can never pull a backend into the thing under
//! test.

use std::collections::{HashMap, HashSet};

use cargo_metadata::{DependencyKind, Metadata, Node, PackageId};

/// Backend / engine / runtime crates that must never appear in `mailmate-core`'s tree.
/// Extend this as adapters are added (each adapter crate's backend goes here).
pub const FORBIDDEN_BACKENDS: &[&str] = &[
    "rusqlite",
    "libsqlite3-sys",
    "sqlx",
    "tokio",
    "tokio-macros",
    "mio",
    "reqwest",
    "hyper",
    "h2",
    "burn",
    "candle-core",
    "ort",
    "onnxruntime",
    "tract-core",
    "pyo3",
    "numpy",
];

/// The only workspace crates `mailmate-core` may depend on.
pub const CORE_ALLOWED_WORKSPACE_DEPS: &[&str] = &["mailmate-ports", "mailmate-common"];

/// A reduced view of one crate's dependencies, sufficient for the guard.
#[derive(Clone, Debug)]
pub struct CrateDeps {
    /// The crate's name.
    pub name: String,
    /// Resolved non-dev (normal/build) direct dependency names.
    pub runtime_dep_names: Vec<String>,
    /// Every declared dependency name, ANY kind (including dev and optional).
    pub all_declared_dep_names: Vec<String>,
}

/// Check `mailmate-core` against the hexagonal law. Returns a list of violation
/// messages (empty = the law holds). `runtime_closure` is the set of crate names
/// reachable from core over non-dev edges.
#[must_use]
pub fn check_core_isolation(core: &CrateDeps, runtime_closure: &[String]) -> Vec<String> {
    let mut violations = Vec::new();

    // (1) Core's direct workspace deps must fall within the allow-list.
    for dep in &core.runtime_dep_names {
        if dep.starts_with("mailmate-") && !CORE_ALLOWED_WORKSPACE_DEPS.contains(&dep.as_str()) {
            violations.push(format!(
                "core depends on workspace crate `{dep}`, but only \
                 {CORE_ALLOWED_WORKSPACE_DEPS:?} are allowed"
            ));
        }
    }

    // (2) No forbidden backend anywhere in core's runtime closure (catches transitive pulls).
    for name in runtime_closure {
        if FORBIDDEN_BACKENDS.contains(&name.as_str()) {
            violations.push(format!(
                "forbidden backend `{name}` appears in core's runtime dependency closure"
            ));
        }
    }

    // (3) No forbidden backend DECLARED at all (catches optional/feature-gated/dev backdoors,
    //     which a default-feature runtime-closure walk is blind to).
    for dep in &core.all_declared_dep_names {
        if FORBIDDEN_BACKENDS.contains(&dep.as_str()) {
            violations.push(format!(
                "forbidden backend `{dep}` is declared as a dependency of core \
                 (any kind, including optional/dev)"
            ));
        }
    }

    violations
}

/// Extract `mailmate-core`'s dependency view and runtime closure from `cargo metadata`.
///
/// # Errors
/// Returns an error string if the metadata lacks `mailmate-core` or a resolve graph.
pub fn extract_core(meta: &Metadata) -> Result<(CrateDeps, Vec<String>), String> {
    let core = meta
        .packages
        .iter()
        .find(|p| p.name.as_str() == "mailmate-core")
        .ok_or_else(|| "mailmate-core package not found in metadata".to_owned())?;

    let all_declared_dep_names = core.dependencies.iter().map(|d| d.name.clone()).collect();
    let runtime_dep_names = core
        .dependencies
        .iter()
        .filter(|d| d.kind != DependencyKind::Development)
        .map(|d| d.name.clone())
        .collect();

    let resolve = meta
        .resolve
        .as_ref()
        .ok_or_else(|| "metadata has no resolve graph (do not pass --no-deps)".to_owned())?;

    let id_to_name: HashMap<PackageId, String> = meta
        .packages
        .iter()
        .map(|p| (p.id.clone(), p.name.to_string()))
        .collect();
    let node_by_id: HashMap<PackageId, &Node> =
        resolve.nodes.iter().map(|n| (n.id.clone(), n)).collect();

    let mut closure = Vec::new();
    let mut seen: HashSet<PackageId> = HashSet::new();
    let mut stack: Vec<PackageId> = vec![core.id.clone()];
    while let Some(id) = stack.pop() {
        let Some(node) = node_by_id.get(&id) else {
            continue;
        };
        for nd in &node.deps {
            let is_runtime = nd
                .dep_kinds
                .iter()
                .any(|dk| dk.kind != DependencyKind::Development);
            if !is_runtime {
                continue;
            }
            if seen.insert(nd.pkg.clone()) {
                if let Some(name) = id_to_name.get(&nd.pkg) {
                    closure.push(name.clone());
                }
                stack.push(nd.pkg.clone());
            }
        }
    }

    Ok((
        CrateDeps {
            name: "mailmate-core".to_owned(),
            runtime_dep_names,
            all_declared_dep_names,
        },
        closure,
    ))
}

/// Returns this crate's package name for smoke tests.
#[must_use]
pub fn crate_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(rt: &[&str], all: &[&str]) -> CrateDeps {
        CrateDeps {
            name: "mailmate-core".to_owned(),
            runtime_dep_names: rt.iter().map(|s| (*s).to_owned()).collect(),
            all_declared_dep_names: all.iter().map(|s| (*s).to_owned()).collect(),
        }
    }
    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn crate_name_is_available() {
        assert_eq!(crate_name(), "mailmate-arch-test");
    }

    #[test]
    fn clean_core_passes() {
        let c = core(&["mailmate-ports", "serde"], &["mailmate-ports", "serde"]);
        let closure = names(&["mailmate-ports", "mailmate-common", "serde", "serde_core"]);
        assert!(check_core_isolation(&c, &closure).is_empty());
    }

    #[test]
    fn core_depending_on_an_adapter_crate_fails() {
        let c = core(&["mailmate-adapter-sqlite"], &["mailmate-adapter-sqlite"]);
        let v = check_core_isolation(&c, &names(&["mailmate-adapter-sqlite"]));
        assert!(
            v.iter().any(|m| m.contains("mailmate-adapter-sqlite")),
            "got {v:?}"
        );
    }

    #[test]
    fn transitive_backend_in_closure_fails() {
        let c = core(&["mailmate-ports"], &["mailmate-ports"]);
        let v = check_core_isolation(&c, &names(&["mailmate-ports", "rusqlite"]));
        assert!(v.iter().any(|m| m.contains("rusqlite")), "got {v:?}");
    }

    #[test]
    fn optional_or_dev_backend_declared_fails() {
        // `tokio` only as a declared (e.g. optional or dev) dep — invisible to a
        // default runtime-closure walk, but caught by the declared-deps scan.
        let c = core(&["mailmate-ports"], &["mailmate-ports", "tokio"]);
        let v = check_core_isolation(&c, &names(&["mailmate-ports"]));
        assert!(v.iter().any(|m| m.contains("tokio")), "got {v:?}");
    }
}
