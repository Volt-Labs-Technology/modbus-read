//! [`FunctionCode`] has exactly two variants. A write cannot be named.
//!
//! The scan reads the enum from source. A third variant would have to be
//! added here in a visible diff, not slipped into a match arm.

use std::fs;
use std::path::Path;

/// The function codes this crate can express. A write is not among them.
#[test]
fn the_only_function_codes_are_the_two_that_read() {
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

    assert_eq!(variants, ["ReadHoldingRegisters", "ReadInputRegisters"]);
}
