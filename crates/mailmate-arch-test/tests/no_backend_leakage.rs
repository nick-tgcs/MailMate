//! Integration enforcement: run real `cargo metadata` and assert `mailmate-core` obeys
//! the no-backend-leakage law on the actual workspace.

use mailmate_arch_test::{check_core_isolation, extract_core};

#[test]
fn mailmate_core_has_no_backend_leakage() {
    let meta = cargo_metadata::MetadataCommand::new()
        .exec()
        .expect("`cargo metadata` should run in the workspace");

    let (core, closure) = extract_core(&meta).expect("mailmate-core present in metadata");
    let violations = check_core_isolation(&core, &closure);

    assert!(
        violations.is_empty(),
        "mailmate-core violates the no-backend-leakage law:\n  {}",
        violations.join("\n  ")
    );
}
