//! Real e2e tests for bufflink spawn the freebuff binary. That binary enforces
//! a single running instance and shares `~/.config/manicode`, so these tests
//! are `#[ignore]`d and only run explicitly with `BUFFLINK_E2E=1`.

/// Placeholder so the e2e test harness compiles and the gate passes; real tests
/// spawn the freebuff binary and need `BUFFLINK_E2E=1`.
#[test]
#[ignore]
fn e2e_placeholder() {}
