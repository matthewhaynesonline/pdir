# `pdir (print directory)`

A small command line utility for concatenating and printing directory files into a single text stream with relative path headers.

Similar to https://github.com/simonw/files-to-prompt, but you know... 🦀.

Useful for:

- Feeding source trees into LLMs
- Creating readable project dumps
- Inspecting source code quickly
- Generating context snapshots
- Sharing compact project references

Example output:

```
--- Cargo.toml ---
[package]
name = "example"

--- src/main.rs ---
fn main() {
    println!("hello");
}
```

## Features

- Recursive directory traversal
- Relative path headers
- Include glob patterns
- Exclude glob patterns
- `.gitignore` support on by default
- UTF-8 lossy reading (won't explode on weird files)
- Single binary

## Installation

### Build Locally

```bash
cargo build --release
```

Binary will be located at:

```
target/release/pdir
```

### Install via Cargo

```bash
cargo install --path .
```

This installs the binary to `~/.cargo/bin`. Make sure that directory is on your `PATH`:

```bash
# Add to ~/.zshrc or ~/.bashrc
export PATH="$HOME/.cargo/bin:$PATH"
```

Verify:

```bash
which pdir
# /Users/yourname/.cargo/bin/pdir
```

## Usage

Run `pdir --help` for full usage. Quick examples:

```bash
pdir .                                          # current directory
pdir . --include '**/*.rs' --include '**/*.md'  # filtered
```

### Suggested zsh alias: clipboard workflow (macOS)

```bash
alias pdir-copy='pdir . | pbcopy'
```

Copies your project tree to the clipboard, ready to paste into a chat interface — one of the most practical ways to give an LLM project context.

## Notes

### Glob Patterns

Patterns use standard glob syntax:

```text
**/target/**
**/*.rs
*.pyc
**/__pycache__/**
```

### Binary Files

Files are read using UTF-8 lossy decoding - text files work normally, binary files won't crash the utility, but binary content may still appear in output. Exclude binary formats explicitly if needed:

```bash
pdir . \
  --exclude '*.png' \
  --exclude '*.jpg' \
  --exclude '*.pdf'
```

Or rely on `.gitignore` if your repo already excludes generated and binary artifacts.

## Future Ideas

- XML output mode (e.g. Claude `<documents>` format)
- Markdown output mode
- Token estimation
- File count / size summary
- Parallel file reading
- Syntax highlighted output
