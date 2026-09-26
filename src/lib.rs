//! Strip a single Rust source file down for submission to a judge.
//!
//! [`strip`] removes tests, doc comments and comments from one source text, and can
//! minify what is left. It works on the text alone and knows nothing about Cargo.
//!
//! Removal edits the text in place instead of printing the syntax tree back out,
//! so whatever is not removed keeps its layout. It also reaches inside
//! `macro_rules!` bodies and macro invocations.
//!
//! ```
//! let code = r#"
//! /// Adds one.
//! fn inc(x: i64) -> i64 {
//!     x + 1 // the answer
//! }
//!
//! #[cfg(test)]
//! mod tests {
//!     #[test]
//!     fn t() {
//!         assert_eq!(super::inc(1), 2);
//!     }
//! }
//! "#;
//!
//! let stripped = rs_strip::strip(code, &rs_strip::Options::default())?;
//! assert_eq!(stripped, "fn inc(x: i64) -> i64 {\n    x + 1\n}\n");
//! # Ok::<_, anyhow::Error>(())
//! ```
//!
//! The building blocks are public too, for tools that layer their own
//! transformations on top: [`SourceEdit`] and its methods, [`minify_tokens`].

#![forbid(unsafe_code)]

mod edit;
mod minify;
mod strip;

pub use crate::{
    edit::{SourceEdit, nested_metas, replace_ranges},
    minify::minify_tokens,
    strip::Cfg,
};

use anyhow::Context as _;
use quote::ToTokens as _;

/// What [`strip`] removes, and whether it minifies.
#[derive(Clone, Debug)]
pub struct Options {
    /// Remove `#[cfg(test)]` items and `#[test]` functions.
    pub strip_tests: bool,
    /// Remove doc comments and `#[doc ..]` attributes.
    pub strip_docs: bool,
    /// Remove `// ..` and `/* .. */` comments.
    pub strip_comments: bool,
    /// Features to treat as enabled. Items behind any other
    /// `#[cfg(feature = "..")]` are removed, since a judge builds a single file
    /// with no features.
    pub features: Vec<String>,
    /// Print the result on one line with as little whitespace as still parses.
    pub minify: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            strip_tests: true,
            strip_docs: true,
            strip_comments: true,
            features: vec![],
            minify: false,
        }
    }
}

/// Applies `options` to `code`, one Rust source file.
pub fn strip(code: &str, options: &Options) -> anyhow::Result<String> {
    let mut edit = SourceEdit::new(code).with_context(|| "could not parse the input")?;

    let features = options
        .features
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    edit.resolve_cfgs(&Cfg {
        features: &features,
        flags: &[],
        strip_tests: options.strip_tests,
    })?;
    if options.strip_docs {
        edit.allow_missing_docs();
        edit.erase_docs()?;
    }
    if options.strip_comments {
        edit.erase_comments()?;
    }
    let code = edit.finish()?;

    if options.minify {
        let file = syn::parse_file(&code).with_context(|| "broke the code during modification")?;
        return Ok(format!("{}\n", minify_tokens(file.to_token_stream())));
    }
    Ok(code)
}
