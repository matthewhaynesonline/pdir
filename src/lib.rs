//! `pdir` - print directory
//!
//! A command line utility that concatenates the files in a directory tree into
//! a single text stream. Each file is preceded by a header and rendered in one
//! of three output formats.
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
//! # Features
//!
//! - Recursive directory traversal via the [`ignore`] crate
//! - `.gitignore` / `.ignore` / global git exclude support (on by default)
//! - Include and exclude glob filtering ([`globset`])
//! - Extension shorthand (`-e rs`)
//! - Plain, Markdown, and Claude `<documents>` XML output formats
//! - Optional 1-based line numbers
//! - Binary file detection with stderr warning (no garbage output)
//! - Buffered stdout for efficient pipeline use

use std::{fmt::Write as _, fs, io::Write, path::Path};

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};

// ── Output format ─────────────────────────────────────────────────────────────

/// Selects how [`print_file`] renders each file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Plain text with configurable header prefix/suffix (default).
    #[default]
    Plain,
    /// GitHub-flavoured Markdown fenced code blocks with language annotation.
    Markdown,
    /// Claude `<documents>` XML wrapper with indexed `<document>` tags.
    ///
    /// The caller is responsible for writing the outer `<documents>` /
    /// `</documents>` wrapper around all [`print_file`] calls.
    Cxml,
}

// ── Print options ─────────────────────────────────────────────────────────────

/// Options that control how [`print_file`] renders a single file.
pub struct PrintOptions<'a> {
    /// Prefix for the plain-text header line (e.g. `"--- "`).
    pub header_prefix: &'a str,
    /// Suffix for the plain-text header line (e.g. `" ---"`).
    pub header_suffix: &'a str,
    /// Output format variant.
    pub format: OutputFormat,
    /// Prepend 1-based line numbers to every content line.
    pub line_numbers: bool,
}

// ── Glob filter ───────────────────────────────────────────────────────────────

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
    #[must_use]
    pub const fn new(includes: Option<GlobSet>, excludes: GlobSet) -> Self {
        Self { includes, excludes }
    }

    /// Returns `true` if the file at `rel` (relative path) with bare `name`
    /// should be included in output.
    #[must_use]
    pub fn allows(&self, rel: &str, name: &str) -> bool {
        if self.excludes.is_match(rel) || self.excludes.is_match(name) {
            return false;
        }

        self.includes.as_ref().is_none_or(|set| set.is_match(rel) || set.is_match(name))
    }
}

// ── Glob utilities ────────────────────────────────────────────────────────────

/// Compiles a list of glob pattern strings into a [`GlobSet`] for efficient
/// multi-pattern matching.
///
/// Returns an error if any pattern is syntactically invalid.
///
/// # Errors
///
/// Returns a [`globset::Error`] if any pattern string fails to parse.
pub fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();

    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
    }

    Ok(builder.build()?)
}

// ── Path utilities ────────────────────────────────────────────────────────────

/// Normalizes a path to use forward slashes.
///
/// On Unix this is a no-op. On Windows, where [`Path`] components are
/// separated by `\`, this ensures consistent output and glob matching across
/// platforms.
#[must_use]
pub fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

// ── File reading ──────────────────────────────────────────────────────────────

/// Reads a file as UTF-8, replacing invalid sequences with `\u{FFFD}`.
///
/// Kept for tests and callers that intentionally want lossy decoding.
/// For normal pipeline use, prefer [`read_text`].
///
/// # Errors
///
/// Returns an error if the file cannot be read (I/O error).
pub fn read_text_lossy(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads a file as strict UTF-8.
///
/// Returns `Ok(None)` and prints a warning to stderr if the file contains
/// invalid UTF-8 (binary content), so the caller can skip it without halting
/// the walk.
///
/// # Errors
///
/// Returns an error if the file cannot be read (I/O error).
pub fn read_text(path: &Path) -> Result<Option<String>> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    String::from_utf8(bytes).map_or_else(
        |_| {
            eprintln!("pdir: skipping binary file: {}", path.display());
            Ok(None)
        },
        |s| Ok(Some(s)),
    )
}

// ── Line numbers ──────────────────────────────────────────────────────────────

/// Prepends 1-based line numbers to every line of `text`.
///
/// Numbers are right-aligned in a field wide enough to hold the largest line
/// number, followed by a tab. A trailing newline is always present.
#[must_use]
pub fn add_line_numbers(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let total = text.lines().count().max(1);
    let width = total.to_string().len();
    let mut buf = String::with_capacity(text.len() + total * (width + 2));
    for (i, line) in text.lines().enumerate() {
        let _ = writeln!(buf, "{:>width$}\t{line}", i + 1);
    }
    buf
}

// ── Language map ──────────────────────────────────────────────────────────────

/// Maps a lowercase file extension to a Markdown fenced-block language tag.
///
/// Returns an empty string for unrecognised extensions, producing an
/// unlabelled fence.
#[must_use]
pub fn ext_to_language(ext: &str) -> &'static str {
    match ext {
        "rs" => "rust",
        "py" | "pyw" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "jsx" => "jsx",
        "tsx" => "tsx",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "scss",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        "yaml" | "yml" => "yaml",
        "xml" => "xml",
        "md" | "mdx" | "markdown" => "markdown",
        "sh" | "bash" | "zsh" | "ksh" => "bash",
        "fish" => "fish",
        "ps1" => "powershell",
        "c" => "c",
        "cpp" | "cc" | "cxx" | "h" | "hpp" | "hxx" => "cpp",
        "go" => "go",
        "java" => "java",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        "lua" => "lua",
        "r" => "r",
        "sql" => "sql",
        "svelte" => "svelte",
        "vue" => "vue",
        "ex" | "exs" => "elixir",
        "hs" | "lhs" => "haskell",
        "nix" => "nix",
        "tf" | "tfvars" => "hcl",
        "proto" => "protobuf",
        "graphql" | "gql" => "graphql",
        _ => "",
    }
}

// ── File printing ─────────────────────────────────────────────────────────────

/// Writes a single file's header and content to `out` in the requested format.
///
/// Returns `true` if the file was written, or `false` if it was skipped
/// (e.g. non-UTF-8 binary). `doc_index` is used only by
/// [`OutputFormat::Cxml`]; pass any value for other formats.
///
/// # Errors
///
/// Returns an error if reading the file or writing to `out` fails.
pub fn print_file(
    out: &mut impl Write,
    path: &Path,
    rel: &str,
    opts: &PrintOptions<'_>,
    doc_index: usize,
) -> Result<bool> {
    let Some(text) = read_text(path)? else { return Ok(false) };

    // Normalise trailing newline before optional line numbering.
    let normalised = if text.ends_with('\n') { text } else { text + "\n" };

    let content = if opts.line_numbers { add_line_numbers(&normalised) } else { normalised };

    match opts.format {
        OutputFormat::Plain => {
            writeln!(out, "{}{rel}{}", opts.header_prefix, opts.header_suffix)?;
            write!(out, "{content}")?;
        }
        OutputFormat::Markdown => {
            let ext = Path::new(rel).extension().and_then(|e| e.to_str()).unwrap_or("");
            let lang = ext_to_language(&ext.to_lowercase());
            writeln!(out, "## {rel}")?;
            writeln!(out, "```{lang}")?;
            write!(out, "{content}")?;
            writeln!(out, "```")?;
        }
        OutputFormat::Cxml => {
            writeln!(out, "<document index=\"{doc_index}\">")?;
            writeln!(out, "<source>{rel}</source>")?;
            writeln!(out, "<document_content>")?;
            write!(out, "{content}")?;
            writeln!(out, "</document_content>")?;
            writeln!(out, "</document>")?;
        }
    }

    Ok(true)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ────────────────────────────────────────────────────────────────

    fn globset(patterns: &[&str]) -> GlobSet {
        build_globset(&patterns.iter().map(ToString::to_string).collect::<Vec<_>>())
            .expect("valid glob pattern")
    }

    fn filter(includes: &[&str], excludes: &[&str]) -> GlobSetFilter {
        let inc = if includes.is_empty() { None } else { Some(globset(includes)) };
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

    // ── read_text ─────────────────────────────────────────────────────────────

    #[test]
    fn read_text_returns_some_for_valid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("valid.txt");
        std::fs::write(&path, "hello\n").unwrap();
        assert_eq!(read_text(&path).unwrap(), Some("hello\n".to_string()));
    }

    #[test]
    fn read_text_returns_none_for_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binary.bin");
        std::fs::write(&path, b"\xff\xfe\x00\x01").unwrap();
        assert_eq!(read_text(&path).unwrap(), None);
    }

    // ── add_line_numbers ──────────────────────────────────────────────────────

    #[test]
    fn line_numbers_single_digit_width() {
        let result = add_line_numbers("a\nb\nc\n");
        assert_eq!(result, "1\ta\n2\tb\n3\tc\n");
    }

    #[test]
    fn line_numbers_pads_to_consistent_width() {
        // 10 lines → width 2
        let text = "x\n".repeat(10);
        let result = add_line_numbers(&text);
        assert!(result.starts_with(" 1\tx\n"));
        assert!(result.contains("10\tx\n"));
    }

    #[test]
    fn line_numbers_empty_input_returns_empty() {
        assert_eq!(add_line_numbers(""), "");
    }

    // ── ext_to_language ───────────────────────────────────────────────────────

    #[test]
    fn known_extensions_map_correctly() {
        assert_eq!(ext_to_language("rs"), "rust");
        assert_eq!(ext_to_language("py"), "python");
        assert_eq!(ext_to_language("ts"), "typescript");
        assert_eq!(ext_to_language("toml"), "toml");
    }

    #[test]
    fn unknown_extension_returns_empty() {
        assert_eq!(ext_to_language("xyz"), "");
        assert_eq!(ext_to_language(""), "");
    }
}
