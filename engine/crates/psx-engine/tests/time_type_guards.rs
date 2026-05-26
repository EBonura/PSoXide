//! Compile-fail guards for engine clock newtypes.

#[test]
fn time_types_reject_semantic_mismatch() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/time_type_mismatch.rs");
}
