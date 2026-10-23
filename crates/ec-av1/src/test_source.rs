#![cfg(test)]
//! Test-only access to a crate module's SOURCE TEXT.
//!
//! The source-scanning gates used to `include_str!("decode.rs")`, which names
//! one file. Once a module is split into a directory (`decode/mod.rs` plus its
//! children) that include stops compiling, and every gate pinning it goes stale
//! on the next move. These helpers read the module's files at test run time
//! instead: `src/<name>.rs` when it exists, then every `*.rs` under
//! `src/<name>/` recursively, concatenated in sorted path order. A file added to
//! that directory is therefore scanned without touching any gate.
//!
//! [`module_production_source`] is the same text minus the test-only files
//! (`tests.rs` and `*_tests.rs`), for gates that must not count their own test
//! fixtures. An inline `mod tests { .. }` in a single file is not removed by
//! that function; a gate that needs the production prefix cuts it itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

type Cache = Mutex<BTreeMap<(String, bool), &'static str>>;
static CACHE: OnceLock<Cache> = OnceLock::new();

/// The whole source text of module `name`: `src/<name>.rs` and every `*.rs`
/// under `src/<name>/`, in sorted path order.
pub fn module_source(name: &str) -> &'static str {
    cached(name, false)
}

/// [`module_source`] without the test-only files (`tests.rs`, `*_tests.rs`).
pub fn module_production_source(name: &str) -> &'static str {
    cached(name, true)
}

/// Each module is read once per test process. The text is leaked, which is
/// bounded by the handful of modules the gates name and is test-only.
fn cached(name: &str, production_only: bool) -> &'static str {
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut map = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = (name.to_string(), production_only);
    if let Some(&text) = map.get(&key) {
        return text;
    }
    let text: &'static str = Box::leak(read_module(name, production_only).into_boxed_str());
    map.insert(key, text);
    text
}

fn read_module(name: &str, production_only: bool) -> String {
    assert!(
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "{name:?} is not a module name"
    );
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<PathBuf> = Vec::new();
    let single = src.join(format!("{name}.rs"));
    if single.is_file() {
        files.push(single);
    }
    let dir = src.join(name);
    if dir.is_dir() {
        collect_rs(&dir, &mut files);
    }
    assert!(
        !files.is_empty(),
        "module {name} has neither src/{name}.rs nor src/{name}/"
    );
    files.sort();
    // A split moved some top-level fns to `pub(super) fn` so sibling modules can call them. The
    // scanners match header text (`\nfn name(`); normalize only that visibility so they see the
    // same headers as before the split. Bodies and assertions are unchanged.
    let mut text = String::new();
    for path in files {
        if production_only && is_test_file(&path) {
            continue;
        }
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        text.push_str(&body);
        if !body.ends_with('\n') {
            text.push('\n');
        }
    }
    text.replace("\npub(super) fn ", "\nfn ").replace("\n    pub(super) fn ", "\n    fn ")
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("reading a directory entry").path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn is_test_file(path: &Path) -> bool {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    stem == "tests" || stem.ends_with("_tests")
}
