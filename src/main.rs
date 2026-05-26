use std::{
    io::{self, BufWriter, Write},
    path::PathBuf,
};

use anyhow::{Result, anyhow};
use clap::{ArgAction, Parser};
use ignore::WalkBuilder;

use pdir::{GlobSetFilter, build_globset, normalize_path, print_file};

#[derive(Parser, Debug)]
#[command(
    name = "pdir",
    version,
    about = "Print directory file contents to stdout with relative path headers."
)]
struct Args {
    /// File or directory to process
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Include glob pattern (can be repeated)
    ///
    /// Only files matching at least one include pattern are printed.
    /// Patterns match against both the relative path and bare filename,
    /// so `*.rs` works without a `**/` prefix.
    ///
    /// Examples:
    ///   pdir . --include '**/*.rs'
    ///   pdir . --include '**/*.rs' --include '**/*.toml'
    #[arg(short, long, action = ArgAction::Append)]
    include: Vec<String>,

    /// Exclude glob pattern (can be repeated)
    ///
    /// Exclusions take priority over includes. Layered on top of .gitignore
    /// when gitignore support is active.
    ///
    /// Examples:
    ///   pdir . --exclude '*.lock'
    ///   pdir . --exclude '**/target/**' --exclude '**/__pycache__/**'
    #[arg(short, long, action = ArgAction::Append)]
    exclude: Vec<String>,

    /// Disable .gitignore, .ignore, and global gitignore when walking
    ///
    /// By default pdir respects ignore files (like ripgrep and fd).
    /// Use this flag to walk all files unconditionally.
    ///
    /// Example:
    ///   pdir . --no-gitignore
    #[arg(long)]
    no_gitignore: bool,

    /// Include hidden files and directories (those starting with `.`)
    ///
    /// By default pdir skips hidden files and directories.
    /// Use this flag to include them.
    ///
    /// Note: this includes `.git`, which is rarely desirable. Pair with
    /// an exclude to avoid it:
    ///   pdir . --hidden --exclude '**/.git/**'
    #[arg(long)]
    hidden: bool,

    /// Header prefix
    #[arg(long, default_value = "--- ")]
    header_prefix: String,

    /// Header suffix
    #[arg(long, default_value = " ---")]
    header_suffix: String,

    /// Print separator blank line between files
    #[arg(long)]
    separator: bool,
}

fn main() -> Result<()> {
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    let args = Args::parse();

    if !args.path.exists() {
        return Err(anyhow!("path does not exist: {}", args.path.display()));
    }

    let exclude_set = build_globset(&args.exclude)?;

    let include_set = if args.include.is_empty() {
        None
    } else {
        Some(build_globset(&args.include)?)
    };

    let filter = GlobSetFilter::new(include_set, exclude_set);

    if args.path.is_file() {
        let name = args.path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if filter.allows(name, name) {
            print_file(
                &mut out,
                &args.path,
                name,
                &args.header_prefix,
                &args.header_suffix,
            )?;
        }

        return Ok(());
    }

    let root = args.path.canonicalize()?;

    let use_gitignore = !args.no_gitignore;

    let walker = WalkBuilder::new(&root)
        .git_ignore(use_gitignore)
        .git_global(use_gitignore)
        .git_exclude(use_gitignore)
        .hidden(!args.hidden)
        .build();

    let mut first = true;

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }

        let rel = normalize_path(entry.path().strip_prefix(&root)?);
        let name = entry.file_name().to_string_lossy();

        if !filter.allows(&rel, &name) {
            continue;
        }

        if args.separator && !first {
            writeln!(out)?;
        }

        first = false;

        print_file(
            &mut out,
            entry.path(),
            &rel,
            &args.header_prefix,
            &args.header_suffix,
        )?;

        writeln!(out)?;
    }

    Ok(())
}
