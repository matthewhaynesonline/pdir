//! `pdir` - print directory
//!
//! A command line utility that concatenates the files in a directory tree into
//! a single text stream, prefixing each file with a relative path header:
//!
//! ```text
//! --- src/main.rs ---
//! fn main() { ... }
//!
//! --- Cargo.toml ---
//! [package]
//! name = "pdir"
//! ```
//!
//! Designed for feeding source trees into LLMs, creating readable project
//! dumps, and shell pipeline workflows.
//!
//! # Features
//!
//! - Recursive directory traversal via the [`ignore`] crate
//! - `.gitignore` / `.ignore` / global git exclude support (on by default)
//! - Include and exclude glob filtering ([`globset`])
//! - Customisable header prefix and suffix
//! - UTF-8 lossy decoding (binary files degrade gracefully)
//! - Buffered stdout for efficient pipeline use

use std::{fs, io::Write, path::Path};

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};

/// Decides whether a file should be included in output based on glob include
/// and exclude sets.
///
/// Each file is tested by both its relative path from the walk root (e.g.
/// `src/lib/util.rs`) and its bare filename (e.g. `util.rs`), so patterns
/// like `*.rs` work without needing a `**/` prefix.
///
/// Exclusions take priority: a file matching both an include and an exclude
/// pattern is always rejected.
pub struct GlobSetFilter {
    includes: Option<GlobSet>,
    excludes: GlobSet,
}

impl GlobSetFilter {
    /// Creates a new filter.
    ///
    /// Pass `None` for `includes` to allow all files that are not excluded.
    pub fn new(includes: Option<GlobSet>, excludes: GlobSet) -> Self {
        Self { includes, excludes }
    }

    /// Returns `true` if the file at `rel` (relative path) with bare `name`
    /// should be included in output.
    pub fn allows(&self, rel: &str, name: &str) -> bool {
        if self.excludes.is_match(rel) || self.excludes.is_match(name) {
            return false;
        }

        match &self.includes {
            Some(set) => set.is_match(rel) || set.is_match(name),
            None => true,
        }
    }
}

/// Compiles a list of glob pattern strings into a [`GlobSet`] for efficient
/// multi-pattern matching.
///
/// Returns an error if any pattern is syntactically invalid.
pub fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();

    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }

    Ok(builder.build()?)
}

/// Normalizes a path to use forward slashes.
///
/// On Unix this is a no-op. On Windows, where [`Path`] components are
/// separated by `\`, this ensures consistent output and glob matching across
/// platforms.
pub fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Reads a file and returns its contents as a `String`.
///
/// Invalid UTF-8 sequences are replaced with the Unicode replacement character
/// (`\u{FFFD}`) rather than returning an error, so binary or mixed-encoding
/// files degrade gracefully instead of halting the walk.
pub fn read_text_lossy(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;

    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Writes a single file's header and content to `out`.
///
/// The header is formatted as `{header_prefix}{rel}{header_suffix}` on its own
/// line, followed by the file's content. A trailing newline is always written
/// after the content so that concatenated output remains well-formed even when
/// source files lack a final newline.
pub fn print_file(
    out: &mut impl Write,
    path: &Path,
    rel: &str,
    header_prefix: &str,
    header_suffix: &str,
) -> Result<()> {
    writeln!(out, "{}{}{}", header_prefix, rel, header_suffix)?;
    writeln!(out, "{}", read_text_lossy(path)?)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ────────────────────────────────────────────────────────────────

    fn globset(patterns: &[&str]) -> GlobSet {
        build_globset(&patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .expect("valid glob pattern")
    }

    fn filter(includes: &[&str], excludes: &[&str]) -> GlobSetFilter {
        let inc = if includes.is_empty() {
            None
        } else {
            Some(globset(includes))
        };

        GlobSetFilter::new(inc, globset(excludes))
    }

    // ── normalize_path ─────────────────────────────────────────────────────────

    #[test]
    fn normalize_path_unix_unchanged() {
        let p = Path::new("src/main.rs");
        assert_eq!(normalize_path(p), "src/main.rs");
    }

    #[test]
    fn normalize_path_converts_backslashes() {
        let p = Path::new("src\\lib\\util.rs");
        assert_eq!(normalize_path(p), "src/lib/util.rs");
    }

    // ── GlobSetFilter::allows ─────────────────────────────────────────────────

    #[test]
    fn no_includes_no_excludes_allows_everything() {
        let f = filter(&[], &[]);
        assert!(f.allows("src/main.rs", "main.rs"));
        assert!(f.allows("Cargo.toml", "Cargo.toml"));
    }

    #[test]
    fn exclude_by_rel_path() {
        let f = filter(&[], &["**/target/**"]);
        assert!(!f.allows("target/release/pdir", "pdir"));
        assert!(f.allows("src/main.rs", "main.rs"));
    }

    #[test]
    fn exclude_by_filename() {
        let f = filter(&[], &["*.lock"]);
        assert!(!f.allows("Cargo.lock", "Cargo.lock"));
        assert!(f.allows("Cargo.toml", "Cargo.toml"));
    }

    #[test]
    fn include_filters_to_matching_files() {
        let f = filter(&["**/*.rs"], &[]);
        assert!(f.allows("src/main.rs", "main.rs"));
        assert!(!f.allows("Cargo.toml", "Cargo.toml"));
    }

    #[test]
    fn include_matches_by_filename_without_path() {
        let f = filter(&["*.toml"], &[]);
        assert!(f.allows("Cargo.toml", "Cargo.toml"));
        assert!(!f.allows("src/main.rs", "main.rs"));
    }

    #[test]
    fn exclude_takes_priority_over_include() {
        let f = filter(&["**/*.rs"], &["**/generated/**"]);
        assert!(!f.allows("src/generated/bindings.rs", "bindings.rs"));
    }

    #[test]
    fn multiple_includes_act_as_union() {
        let f = filter(&["**/*.rs", "**/*.toml"], &[]);
        assert!(f.allows("src/main.rs", "main.rs"));
        assert!(f.allows("Cargo.toml", "Cargo.toml"));
        assert!(!f.allows("README.md", "README.md"));
    }

    // ── build_globset ─────────────────────────────────────────────────────────

    #[test]
    fn build_globset_invalid_pattern_errors() {
        let result = build_globset(&["[invalid".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn build_globset_empty_matches_nothing() {
        let gs = build_globset(&[]).unwrap();
        assert!(!gs.is_match("anything.rs"));
    }

    // ── read_text_lossy ───────────────────────────────────────────────────────

    #[test]
    fn read_text_lossy_reads_utf8_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, "hello world\n").unwrap();
        assert_eq!(read_text_lossy(&path).unwrap(), "hello world\n");
    }

    #[test]
    fn read_text_lossy_survives_non_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binary.bin");
        std::fs::write(&path, b"\xff\xfe binary \x00\x01").unwrap();
        assert!(read_text_lossy(&path).is_ok());
    }

    #[test]
    fn read_text_lossy_missing_file_errors() {
        let result = read_text_lossy(Path::new("/nonexistent/path/file.txt"));
        assert!(result.is_err());
    }
}
