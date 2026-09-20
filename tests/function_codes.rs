//! The two named read function codes stay in [`FunctionCode`].
//!
//! Further variants are growth. Removing either named read is a contract
//! break. A write still cannot hide in production bytes; that is
//! `tests/read_only.rs` and `tests/contract.rs`.

use std::fs;
use std::path::Path;

/// The published read function codes remain spellable.
#[test]
fn the_named_read_function_codes_remain() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/frame.rs");
    let source =
        fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let variants = source
        .lines()
        .skip_while(|line| !line.contains("pub enum FunctionCode"))
        .skip(1)
        .take_while(|line| !line.contains('}'))
        .map(str::trim)
        .filter(|line| line.ends_with(',') && !line.starts_with("///") && !line.starts_with("//"))
        .map(|line| line.trim_end_matches(',').to_owned())
        .collect::<Vec<String>>();

    assert!(
        variants.iter().any(|name| name == "ReadHoldingRegisters"),
        "ReadHoldingRegisters missing from FunctionCode: {variants:?}"
    );
    assert!(
        variants.iter().any(|name| name == "ReadInputRegisters"),
        "ReadInputRegisters missing from FunctionCode: {variants:?}"
    );
}
