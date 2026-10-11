#![forbid(unsafe_code)]

//! Workspace gates. The tests in `tests/gates.rs` read the repository tree
//! directly: `#![forbid(unsafe_code)]` on every crate root, `unsafe` only within
//! `unsafe-budget.toml`, and the `features.toml` registry.
