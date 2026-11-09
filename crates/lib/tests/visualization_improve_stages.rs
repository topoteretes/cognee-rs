#![cfg(feature = "visualization")]
//! The visualization catalog's improve rows cannot drift from the improve
//! registry.
//!
//! Port of Python's `test_operations_catalog.py` registry assertions. Python
//! generates the improve rows by walking `DEFAULT_STAGES`; the Rust
//! `cognee-visualization` crate cannot depend on this crate (the dependency
//! runs the other way), so its `IMPROVE_STAGE_DESCRIPTORS` carry each stage's
//! name and `needs_sessions` themselves, and this test pins both to
//! `crates/lib/src/api/improve/stages.rs`.

use cognee::api::improve::{DEFAULT_STAGES, StageKind};
use cognee::visualization::preprocessor::operations_catalog::{
    IMPROVE_STAGE_DESCRIPTORS, get_operations_catalog, iter_improve_operations,
};

#[test]
fn every_registry_stage_has_exactly_one_descriptor_in_registry_order() {
    let registry: Vec<&str> = DEFAULT_STAGES.iter().map(|stage| stage.name()).collect();
    let descriptors: Vec<&str> = IMPROVE_STAGE_DESCRIPTORS
        .iter()
        .map(|descriptor| descriptor.name)
        .collect();
    assert_eq!(descriptors, registry);
}

#[test]
fn scope_comes_from_needs_sessions() {
    for descriptor in IMPROVE_STAGE_DESCRIPTORS {
        let stage = StageKind::from_name(descriptor.name)
            .unwrap_or_else(|| panic!("{} is not an improve stage", descriptor.name));
        assert_eq!(
            descriptor.needs_sessions,
            stage.needs_sessions(),
            "{}: needs_sessions drifted from the registry",
            descriptor.name
        );
        let expected_scope = if stage.needs_sessions() {
            "subset"
        } else {
            "whole"
        };
        assert_eq!(descriptor.scope(), expected_scope);
    }
}

#[test]
fn catalog_appends_one_row_per_registry_stage() {
    let rows = iter_improve_operations();
    let names: Vec<&str> = rows
        .iter()
        .map(|row| row["name"].as_str().unwrap_or_default())
        .collect();
    let registry: Vec<&str> = DEFAULT_STAGES.iter().map(|stage| stage.name()).collect();
    assert_eq!(names, registry);

    let catalog: Vec<String> = get_operations_catalog()
        .iter()
        .map(|op| op["name"].as_str().unwrap_or_default().to_string())
        .collect();
    for name in registry {
        assert!(catalog.iter().any(|op| op == name), "{name} missing");
    }
}
