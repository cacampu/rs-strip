use anyhow::Context as _;
use clap::Parser;
use std::{
    fs,
    io::{self, Read as _, Write as _},
    path::PathBuf,
};

/// Strip a Rust source file down for submission to a judge.
///
/// By default removes `#[cfg(test)]` items, `#[test]` functions, doc comments and
/// comments, and keeps everything else as written.
#[derive(Parser)]
#[command(version)]
struct Opt {
    /// The source file. Reads stdin when omitted or `-`
    file: Option<PathBuf>,

    /// Write to this file instead of stdout
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// Keep `#[cfg(test)]` items and `#[test]` functions
    #[arg(long)]
    keep_tests: bool,

    /// Keep doc comments
    #[arg(long)]
    keep_docs: bool,

    /// Keep comments
    #[arg(long)]
    keep_comments: bool,

    /// Features to treat as enabled. Items behind other `#[cfg(feature = "..")]`
    /// are removed
    #[arg(long, value_name = "FEATURES", value_delimiter = ',')]
    features: Vec<String>,

    /// Also minify the result onto one line
    #[arg(short, long)]
    minify: bool,
}

fn main() -> anyhow::Result<()> {
    let opt = Opt::parse();

    let code = match opt.file.as_deref() {
        None => read_stdin()?,
        Some(path) if path.as_os_str() == "-" => read_stdin()?,
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("could not read `{}`", path.display()))?,
    };

    let stripped = rs_strip::strip(
        &code,
        &rs_strip::Options {
            strip_tests: !opt.keep_tests,
            strip_docs: !opt.keep_docs,
            strip_comments: !opt.keep_comments,
            features: opt.features,
            minify: opt.minify,
        },
    )?;

    match opt.output {
        Some(path) => fs::write(&path, stripped)
            .with_context(|| format!("could not write `{}`", path.display()))?,
        None => io::stdout().write_all(stripped.as_bytes())?,
    }
    Ok(())
}

fn read_stdin() -> anyhow::Result<String> {
    let mut code = String::new();
    io::stdin()
        .read_to_string(&mut code)
        .with_context(|| "could not read stdin")?;
    Ok(code)
}
