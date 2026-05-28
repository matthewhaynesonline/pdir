use std::{
    fs::File,
    io::{self, BufWriter, IsTerminal, Read, Write},
    path::PathBuf,
};

use anyhow::{Context, Result, anyhow};
use clap::{
    ArgAction, Parser,
    builder::styling::{AnsiColor, Effects, Styles},
};
use ignore::WalkBuilder;

use pdir::{GlobSetFilter, OutputFormat, PrintOptions, build_globset, normalize_path, print_file};

// ── CLI styling ───────────────────────────────────────────────────────────────

/// Clap terminal styling.
fn clap_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Green.on_default() | Effects::BOLD)
        .usage(AnsiColor::Green.on_default() | Effects::BOLD)
        .literal(AnsiColor::Cyan.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::Cyan.on_default())
        .error(AnsiColor::Red.on_default() | Effects::BOLD)
        .valid(AnsiColor::Cyan.on_default() | Effects::BOLD)
        .invalid(AnsiColor::Yellow.on_default() | Effects::BOLD)
}

// ── Args ──────────────────────────────────────────────────────────────────────

#[allow(clippy::struct_excessive_bools)]
#[derive(Parser, Debug)]
#[command(
    name = "pdir",
    version,
    about = "Print directory file contents to stdout with relative path headers.",
    styles = clap_styles(),
)]
struct Args {
    /// Files or directories to process (repeatable; defaults to `.`)
    ///
    /// Additional paths are also accepted from stdin when stdin is not a tty
    /// (one path per line, or NUL-delimited with `--null`).
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,

    /// Include glob pattern (repeatable)
    ///
    /// Only files matching at least one include pattern are printed.
    /// Patterns match against both the relative path and the bare filename,
    /// so `*.rs` works without a `**/` prefix.
    ///
    /// Examples:
    ///   pdir . --include '**/*.rs'
    ///   pdir . --include '**/*.rs' --include '**/*.toml'
    #[arg(short, long, action = ArgAction::Append)]
    include: Vec<String>,

    /// Exclude glob pattern (repeatable)
    ///
    /// Exclusions take priority over includes, layered on top of .gitignore
    /// when gitignore support is active.
    ///
    /// Examples:
    ///   pdir . --exclude '*.lock'
    ///   pdir . --exclude '**/target/**' --exclude '**/__pycache__/**'
    #[arg(short, long, action = ArgAction::Append)]
    exclude: Vec<String>,

    /// Filter by extension, e.g. `-e rs -e toml` (repeatable)
    ///
    /// Shorthand for `--include '**/*.EXT'`. Stacks with explicit `--include`
    /// patterns.
    #[arg(short = 'e', long, action = ArgAction::Append, value_name = "EXT")]
    extension: Vec<String>,

    /// Output in Markdown format (fenced code blocks with language annotation)
    #[arg(long, conflicts_with = "cxml")]
    markdown: bool,

    /// Output in Claude cxml format (`<documents>` wrapper with indexed `<document>` tags)
    #[arg(long, conflicts_with = "markdown")]
    cxml: bool,

    /// Prepend 1-based line numbers to every line of file content
    #[arg(short = 'n', long)]
    line_numbers: bool,

    /// Write output to FILE instead of stdout
    #[arg(short = 'o', long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Read stdin paths as NUL-delimited (`\0`) instead of newline-delimited
    ///
    /// Pairs well with `find -print0` or `fd --print0`.
    #[arg(short = '0', long)]
    null: bool,

    /// Disable .gitignore, .ignore, and global gitignore when walking
    ///
    /// By default pdir respects ignore files (like ripgrep and fd).
    /// Use this flag to walk all files unconditionally.
    #[arg(long)]
    no_gitignore: bool,

    /// Include hidden files and directories (those starting with `.`)
    ///
    /// Note: this includes `.git`, which is rarely desirable. Pair with
    /// an exclude to avoid it:
    ///   pdir . --hidden --exclude '**/.git/**'
    #[arg(long)]
    hidden: bool,

    /// Header prefix (plain format only)
    #[arg(long, default_value = "--- ")]
    header_prefix: String,

    /// Header suffix (plain format only)
    #[arg(long, default_value = " ---")]
    header_suffix: String,

    /// Print a blank line between files (plain format only)
    #[arg(long)]
    separator: bool,
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = Args::parse();

    // ── Output writer ──────────────────────────────────────────────────────────

    let mut out: Box<dyn Write> = match &args.output {
        Some(path) => Box::new(BufWriter::new(
            File::create(path)
                .with_context(|| format!("creating output file: {}", path.display()))?,
        )),
        None => Box::new(BufWriter::new(io::stdout())),
    };

    // ── Format + print options ─────────────────────────────────────────────────

    let format = if args.markdown {
        OutputFormat::Markdown
    } else if args.cxml {
        OutputFormat::Cxml
    } else {
        OutputFormat::Plain
    };

    let opts = PrintOptions {
        header_prefix: &args.header_prefix,
        header_suffix: &args.header_suffix,
        format,
        line_numbers: args.line_numbers,
    };

    // ── Glob filter ────────────────────────────────────────────────────────────

    // Merge -e/--extension into the include list as glob patterns.
    let mut raw_includes = args.include.clone();
    for ext in &args.extension {
        raw_includes.push(format!("**/*.{ext}"));
    }
    let include_set =
        if raw_includes.is_empty() { None } else { Some(build_globset(&raw_includes)?) };
    let exclude_set = build_globset(&args.exclude)?;
    let filter = GlobSetFilter::new(include_set, exclude_set);

    // ── Path collection ────────────────────────────────────────────────────────

    // Start with CLI positional args, then append any paths piped via stdin.
    let mut paths = args.paths.clone();
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        let mut input = String::new();
        stdin.lock().read_to_string(&mut input)?;
        let extra = if args.null {
            input.split('\0').filter(|s| !s.is_empty()).map(PathBuf::from).collect::<Vec<_>>()
        } else {
            input.lines().filter(|s| !s.is_empty()).map(PathBuf::from).collect::<Vec<_>>()
        };
        paths.extend(extra);
    }

    for path in &paths {
        if !path.exists() {
            return Err(anyhow!("path does not exist: {}", path.display()));
        }
    }

    // ── Walk + render ──────────────────────────────────────────────────────────

    let use_gitignore = !args.no_gitignore;

    if format == OutputFormat::Cxml {
        writeln!(out, "<documents>")?;
    }

    let mut doc_index: usize = 1;
    let mut first = true;

    for path in &paths {
        if path.is_file() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !filter.allows(name, name) {
                continue;
            }
            if format == OutputFormat::Plain && args.separator && !first {
                writeln!(out)?;
            }
            if print_file(&mut out, path, name, &opts, doc_index)? {
                doc_index += 1;
                first = false;
                if format == OutputFormat::Plain {
                    writeln!(out)?;
                }
            }
        } else if path.is_dir() {
            let root = path.canonicalize()?;

            let walker = WalkBuilder::new(&root)
                .git_ignore(use_gitignore)
                .git_global(use_gitignore)
                .git_exclude(use_gitignore)
                .hidden(!args.hidden)
                .build();

            for entry in walker {
                let Ok(entry) = entry else { continue };

                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    continue;
                }

                let rel = normalize_path(entry.path().strip_prefix(&root)?);
                let name = entry.file_name().to_string_lossy();

                if !filter.allows(&rel, &name) {
                    continue;
                }

                if format == OutputFormat::Plain && args.separator && !first {
                    writeln!(out)?;
                }

                if print_file(&mut out, entry.path(), &rel, &opts, doc_index)? {
                    doc_index += 1;
                    first = false;
                    if format == OutputFormat::Plain {
                        writeln!(out)?;
                    }
                }
            }
        }
    }

    if format == OutputFormat::Cxml {
        writeln!(out, "</documents>")?;
    }

    out.flush()?;
    Ok(())
}
