//! docs.rs documents `TestServer` when the named feature is on.
//!
//! Default rustdoc must not list `TestServer` as a crate-root item. The
//! `[package.metadata.docs.rs]` table turns `test-server` on for that build.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

const TEST_SERVER_HTML: &str = "struct.TestServer.html";
const DOCS_RS_HEADER: &str = "[package.metadata.docs.rs]";

/// Slice of `toml` from `[package.metadata.docs.rs]` to the next table.
///
/// A `features` list contains `[`, so the next table is a `[` that starts a line.
fn docs_rs_table(toml: &str) -> Option<&str> {
    let start = toml.find(DOCS_RS_HEADER)?;
    let after_header = start + DOCS_RS_HEADER.len();
    let end = toml[after_header..]
        .find("\n[")
        .map_or(toml.len(), |offset| after_header + offset + 1);
    Some(&toml[start..end])
}

fn uncommented(line: &str) -> &str {
    line.split_once('#').map_or(line, |(code, _)| code).trim()
}

fn assignment<'a>(table: &'a str, key: &str) -> Option<&'a str> {
    table.lines().find_map(|line| {
        let (left, right) = uncommented(line).split_once('=')?;
        (left.trim() == key).then_some(right.trim())
    })
}

fn quoted_items(array: &str) -> impl Iterator<Item = &str> {
    let inner = array
        .strip_prefix('[')
        .and_then(|body| body.strip_suffix(']'))
        .unwrap_or("");
    inner
        .split(',')
        .filter_map(|item| item.trim().strip_prefix('"')?.strip_suffix('"'))
}

fn features_name_test_server(table: &str) -> bool {
    assignment(table, "features")
        .is_some_and(|value| quoted_items(value).any(|item| item == "test-server"))
}

fn all_features_true(table: &str) -> bool {
    assignment(table, "all-features") == Some("true")
}

/// Whether the docs.rs metadata table enables the `test-server` feature.
///
/// The parser accepts a named `features` list or `all-features = true`.
/// This crate ships the named-feature form.
fn docs_rs_enables_test_server(toml: &str) -> bool {
    docs_rs_table(toml)
        .is_some_and(|table| features_name_test_server(table) || all_features_true(table))
}

fn unique_target_dir(label: &str) -> PathBuf {
    let pid = std::process::id();
    let base = std::env::temp_dir();
    for n in 0..u32::MAX {
        let dir = base.join(format!("modbus-read-doc-{label}-{pid}-{n}"));
        match fs::create_dir(&dir) {
            Ok(()) => return dir,
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(err) => panic!("create {}: {err}", dir.display()),
        }
    }
    panic!("could not allocate a unique --target-dir for {label}");
}

fn run_rustdoc(target_dir: &Path, extra: &[&str]) {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .arg("doc")
        .arg("--no-deps")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--target-dir")
        .arg(target_dir)
        .args(extra)
        .output()
        .unwrap_or_else(|err| panic!("spawn cargo doc: {err}"));
    assert!(
        output.status.success(),
        "cargo doc failed ({})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn read_index(crate_root: &Path) -> String {
    let path = crate_root.join("index.html");
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// Isolated `cargo doc` output. Drop removes the target dir so leftovers cannot lie.
struct IsolatedDocs {
    target_dir: PathBuf,
}

impl IsolatedDocs {
    fn build(label: &str, extra: &[&str]) -> Self {
        let docs = Self {
            target_dir: unique_target_dir(label),
        };
        run_rustdoc(&docs.target_dir, extra);
        docs
    }

    fn crate_root(&self) -> PathBuf {
        self.target_dir.join("doc").join("modbus_read")
    }
}

impl Drop for IsolatedDocs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.target_dir);
    }
}

#[test]
fn default_rustdoc_omits_test_server_as_a_crate_root_item() {
    let docs = IsolatedDocs::build("default", &[]);
    let root = docs.crate_root();
    let page = root.join(TEST_SERVER_HTML);
    assert!(!page.exists(), "default rustdoc wrote {}", page.display());
    let index = read_index(&root);
    assert!(
        !index.contains(TEST_SERVER_HTML),
        "default index.html hrefs {TEST_SERVER_HTML}"
    );
}

#[test]
fn rustdoc_with_test_server_documents_test_server_as_a_crate_root_item() {
    let docs = IsolatedDocs::build("with-test-server", &["--features", "test-server"]);
    let root = docs.crate_root();
    let page = root.join(TEST_SERVER_HTML);
    assert!(page.is_file(), "rustdoc did not write {}", page.display());
    let index = read_index(&root);
    assert!(
        index.contains(TEST_SERVER_HTML),
        "index.html does not href {TEST_SERVER_HTML}"
    );
}

#[test]
fn docs_rs_metadata_enables_test_server() {
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", manifest_path.display()));
    let table = docs_rs_table(&manifest)
        .unwrap_or_else(|| panic!("{DOCS_RS_HEADER} missing from {}", manifest_path.display()));
    assert!(
        features_name_test_server(table),
        "docs.rs metadata must name the test-server feature"
    );

    // Synthetic Cargo.toml snippets. They are not a real package.
    let snippets: &[(&str, bool)] = &[
        ("", false),
        ("[features]\ntest-server = []\n", false),
        ("[package.metadata.docs.rs]\nfeatures = []\n", false),
        (
            "[package.metadata.docs.rs]\nfeatures = [\"other\"]\n",
            false,
        ),
        ("[package.metadata.docs.rs]\nall-features = false\n", false),
        (
            "[package.metadata.docs.rs]\nfeatures = [\"test-server\"]\n",
            true,
        ),
        ("[package.metadata.docs.rs]\nall-features = true\n", true),
    ];
    for (toml, enabled) in snippets {
        assert_eq!(
            docs_rs_enables_test_server(toml),
            *enabled,
            "synthetic snippet:\n{toml}"
        );
    }
}
