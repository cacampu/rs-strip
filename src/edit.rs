//! The editing core: a source text kept in step with its parsed [`syn::File`],
//! plus a queue of span replacements applied to the text.
//!
//! Ported from `CodeEdit` in [cargo-equip](https://github.com/qryxip/cargo-equip)
//! by Ryo Yamashita (qryxip), MIT OR Apache-2.0.

use anyhow::Context as _;
use fixedbitset::FixedBitSet;
use if_chain::if_chain;
use proc_macro2::{LineColumn, Span};
use std::{collections::BTreeMap, mem};
use syn::{Meta, MetaList, Token, punctuated::Punctuated};

/// Rust source text together with its syntax tree.
///
/// Transformations locate what to change on the tree (`file`) but change the text
/// (`string`), so everything they do not touch keeps its original layout, comments
/// included. Replacements are queued in `replacements`, keyed by the span they
/// cover, and take effect on [`apply`](Self::apply), which also reparses `file`
/// from the new text.
///
/// The fields are public so that a caller layering its own transformations on top
/// can borrow them independently. Keep them consistent: after writing to `string`
/// directly, call [`force_apply`](Self::force_apply).
pub struct SourceEdit {
    pub string: String,
    pub file: syn::File,
    pub replacements: BTreeMap<(LineColumn, LineColumn), String>,
}

impl SourceEdit {
    pub fn new(code: &str) -> syn::Result<Self> {
        Ok(Self {
            file: syn::parse_file(code)?,
            string: code.to_owned(),
            replacements: BTreeMap::new(),
        })
    }

    /// Applies what is still queued and returns the text.
    pub fn finish(mut self) -> anyhow::Result<String> {
        self.apply()?;
        Ok(self.string)
    }

    /// Applies the queued replacements, if there are any.
    pub fn apply(&mut self) -> anyhow::Result<()> {
        if !self.replacements.is_empty() {
            self.force_apply()?;
        }
        Ok(())
    }

    /// Applies the queued replacements and reparses, even if nothing is queued.
    pub fn force_apply(&mut self) -> anyhow::Result<()> {
        self.string = replace_ranges(&self.string, mem::take(&mut self.replacements));
        self.file =
            syn::parse_file(&self.string).with_context(|| "broke the code during modification")?;
        Ok(())
    }
}

/// Replaces each `(start, end)` range of `code` with its string. An empty range
/// inserts before `start`.
pub fn replace_ranges(
    code: &str,
    replacements: BTreeMap<(LineColumn, LineColumn), String>,
) -> String {
    if replacements.is_empty() {
        return code.to_owned();
    }
    let replacements = replacements.into_iter().collect::<Vec<_>>();
    let mut replacements = &*replacements;
    let mut skip_until = None;
    let mut ret = "".to_owned();
    let mut lines = code.trim_end().split('\n').enumerate().peekable();
    while let Some((i, s)) = lines.next() {
        for (j, c) in s.chars().enumerate() {
            if_chain! {
                if let Some(((start, end), replacement)) = replacements.first();
                if (i, j) == (start.line - 1, start.column);
                then {
                    ret += replacement;
                    if start == end {
                        ret.push(c);
                    } else {
                        skip_until = Some(*end);
                    }
                    replacements = &replacements[1..];
                } else {
                    if !matches!(skip_until, Some(LineColumn { line, column }) if (i, j) < (line - 1, column)) {
                        ret.push(c);
                        skip_until = None;
                    }
                }
            }
        }
        while let Some(((start, end), replacement)) = replacements.first() {
            if i == start.line - 1 {
                ret += replacement;
                if start < end {
                    skip_until = Some(*end);
                }
                replacements = &replacements[1..];
            } else {
                break;
            }
        }
        if lines.peek().is_some() || code.ends_with('\n') {
            ret += "\n";
        }
    }
    ret
}

/// The comma-separated `Meta`s inside `#[name(..)]`.
///
/// syn keeps the contents of an attribute list as raw tokens, so anything wanting
/// to look inside has to parse them. An unparseable list yields nothing rather
/// than an error: these are best-effort inspections of other people's attributes.
pub fn nested_metas(list: &MetaList) -> Punctuated<Meta, Token![,]> {
    list.parse_args_with(Punctuated::parse_terminated)
        .unwrap_or_default()
}

/// Sets (`p = true`) or clears (`p = false`) the bits under `span`, one
/// [`FixedBitSet`] per line.
pub(crate) fn set_span(mask: &mut [FixedBitSet], span: Span, p: bool) {
    let i1 = span.start().line - 1;
    if span.start().line == span.end().line {
        let l = span.start().column;
        let r = span.end().column;
        mask[i1].set_range(l..r, p);
    } else {
        let i2 = span.end().line - 1;
        let l = span.start().column;
        mask[i1].set_range(l.., p);
        for mask in &mut mask[i1 + 1..i2] {
            mask.set_range(.., p);
        }
        let r = span.end().column;
        mask[i2].set_range(..r, p);
    }
}
