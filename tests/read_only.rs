//! Write function-code bytes do not appear in production source.
//!
//! Function codes 0x05, 0x06, 0x0f and 0x10 write. They may be named in
//! tests and comments. They must not appear in the code that runs.

use std::fs;
use std::path::{Path, PathBuf};

/// Hex spellings of the write function codes.
const WRITE_CODES: &[&str] = &["0x05", "0x06", "0x0f", "0x0F", "0x10"];

fn crate_sources() -> Vec<PathBuf> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let entries = fs::read_dir(&src).unwrap_or_else(|err| panic!("read {}: {err}", src.display()));
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();
    files
}

fn source_of(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// Production lines: no comment lines, and nothing inside a `#[cfg(test)]`
/// block. Brace-counted so code after a test module is still scanned.
fn production_lines(source: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
    let mut in_test = false;
    let mut depth = 0_i32;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if !in_test && trimmed.starts_with("#[cfg(test)]") {
            in_test = true;
            depth = 0;
            continue;
        }
        if in_test {
            depth += count_of(line, '{') - count_of(line, '}');
            if depth <= 0 {
                in_test = false;
            }
            continue;
        }
        if !trimmed.starts_with("//") {
            lines.push((index + 1, line));
        }
    }
    lines
}

fn count_of(line: &str, brace: char) -> i32 {
    i32::try_from(line.matches(brace).count()).unwrap_or(i32::MAX)
}

/// `.expect(` and `.unwrap(` on a production line. `unwrap_or_else` and
/// `expect_err` are other methods; they do not contain these tokens.
fn panic_token(line: &str) -> Option<&'static str> {
    if line.contains(".expect(") {
        Some(".expect(")
    } else if line.contains(".unwrap(") {
        Some(".unwrap(")
    } else {
        None
    }
}

#[test]
fn the_scan_cannot_pass_vacuously() {
    let files = crate_sources();
    let names: Vec<String> = files
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();

    for expected in ["lib.rs", "frame.rs", "decode.rs", "server.rs"] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} was not among the scanned files: {names:?}"
        );
    }
}

#[test]
fn the_scan_keeps_production_code_and_drops_the_rest() {
    let source = concat!(
        "//! a doc comment\n",
        "fn kept() {}\n",
        "// a line comment\n",
        "#[cfg(test)]\n",
        "mod tests {\n",
        "    fn nested() { let _ = \"0x06\"; }\n",
        "}\n",
        "fn also_kept() {}\n",
    );

    let lines = production_lines(source);

    assert_eq!(lines, vec![(2, "fn kept() {}"), (8, "fn also_kept() {}")]);
}

#[test]
fn panic_tokens_hit_production_and_ignore_the_rest() {
    let source = concat!(
        "fn boom() { x.expect(\"a\"); }\n",
        "fn also() { y.unwrap(); }\n",
        "// a line comment with .expect(\"no\") and .unwrap()\n",
        "#[cfg(test)]\n",
        "mod tests {\n",
        "    fn nested() { z.expect(\"t\"); z.unwrap(); }\n",
        "}\n",
        "fn poison() { lock.unwrap_or_else(into_inner); }\n",
        "fn check() { result.expect_err(\"should fail\"); }\n",
        "fn still_kept() {}\n",
    );

    let hits: Vec<(usize, &str, &str)> = production_lines(source)
        .into_iter()
        .filter_map(|(number, line)| panic_token(line).map(|token| (number, line, token)))
        .collect();

    assert_eq!(
        hits,
        vec![
            (1, "fn boom() { x.expect(\"a\"); }", ".expect("),
            (2, "fn also() { y.unwrap(); }", ".unwrap("),
        ]
    );
}

#[test]
fn server_production_has_no_expect_or_unwrap() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server.rs");
    let source = source_of(&path);
    let hits: Vec<String> = production_lines(&source)
        .into_iter()
        .filter_map(|(number, line)| {
            panic_token(line).map(|token| format!("{}:{number}: {token}\n{line}", path.display()))
        })
        .collect();

    assert!(
        hits.is_empty(),
        "production expect/unwrap in src/server.rs:\n{}",
        hits.join("\n")
    );
}

#[test]
fn no_write_function_code_bytes_in_production() {
    for path in crate_sources() {
        let source = source_of(&path);
        for (number, line) in production_lines(&source) {
            for spelling in WRITE_CODES {
                assert!(
                    !line.contains(spelling),
                    "{}:{number} spells {spelling:?}\n{line}",
                    path.display()
                );
            }
        }
    }
}
