//! Removing what a judge does not need: items whose `#[cfg(..)]` is false for the
//! build, doc comments, and ordinary comments.
//!
//! All three work the same way. They pick spans to drop, and [`SourceEdit::erase`]
//! blanks those spans in the text and tidies the lines left empty. Nothing else in
//! the text moves.
//!
//! Ported from [cargo-equip](https://github.com/qryxip/cargo-equip) by Ryo Yamashita
//! (qryxip), MIT OR Apache-2.0. Compared to the original, items removed by
//! `resolve_cfgs` no longer leave blank lines behind, emptied lines are dropped,
//! and two bugs are fixed: a file starting with an inner attribute lost its first
//! line, and removing comments blanked out the first line of a multi-line string
//! literal.

use crate::edit::{SourceEdit, nested_metas, set_span};
use anyhow::{Context as _, anyhow};
use fixedbitset::FixedBitSet;
use itertools::Itertools as _;
use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};
use quote::ToTokens;
use std::{borrow::Cow, collections::BTreeSet};
use syn::{
    Arm, Attribute, ConstParam, ExprArray, ExprAssign, ExprAsync, ExprAwait, ExprBinary, ExprBlock,
    ExprBreak, ExprCall, ExprCast, ExprClosure, ExprContinue, ExprField, ExprForLoop, ExprGroup,
    ExprIf, ExprIndex, ExprLet, ExprLit, ExprLoop, ExprMacro, ExprMatch, ExprMethodCall, ExprParen,
    ExprPath, ExprRange, ExprReference, ExprRepeat, ExprReturn, ExprStruct, ExprTry, ExprTryBlock,
    ExprTuple, ExprUnary, ExprUnsafe, ExprWhile, ExprYield, Field, FieldPat, FieldValue,
    FnPtrVariadic, ForeignItemFn, ForeignItemMacro, ForeignItemStatic, ForeignItemType,
    ImplItemConst, ImplItemFn, ImplItemMacro, ImplItemType, ItemConst, ItemEnum, ItemExternCrate,
    ItemFn, ItemForeignMod, ItemImpl, ItemMacro, ItemMod, ItemStatic, ItemStruct, ItemTrait,
    ItemTraitAlias, ItemType, ItemUnion, ItemUse, LifetimeParam, Local, Meta, MetaList, NamedArg,
    PatIdent, PatOr, PatReference, PatRest, PatSlice, PatStruct, PatTuple, PatTupleStruct, PatType,
    PatWild, Receiver, TraitItemConst, TraitItemFn, TraitItemMacro, TraitItemType, TypeParam,
    Variadic, Variant,
    spanned::Spanned,
    visit::{self, Visit},
};

/// What [`SourceEdit::resolve_cfgs`] may assume about the build the code is for.
///
/// `proc_macro` is always false. Anything not settled here (`target_os`,
/// `debug_assertions`, flags not listed, ...) is left undecided, and an item whose
/// `#[cfg(..)]` stays undecided is kept as it is.
#[derive(Clone, Copy, Debug)]
pub struct Cfg<'a> {
    /// Features treated as enabled. Every other `feature = ".."` is disabled.
    pub features: &'a [&'a str],
    /// Bare flags treated as set, like `cfg(my_flag)`.
    pub flags: &'a [&'a str],
    /// Treat `cfg(test)` as false, which also drops `#[test]` functions. With
    /// `false`, `cfg(test)` is left undecided and tests are kept.
    pub strip_tests: bool,
}

impl Default for Cfg<'_> {
    fn default() -> Self {
        Self {
            features: &[],
            flags: &[],
            strip_tests: true,
        }
    }
}

impl SourceEdit {
    /// Drops every item whose `#[cfg(..)]` is false, and every `#[cfg(..)]` that is
    /// true (the item stays, the attribute goes).
    ///
    /// With [`Cfg::strip_tests`], items marked `#[test]` go too: outside a test
    /// build the compiler discards them anyway.
    pub fn resolve_cfgs(&mut self, cfg: &Cfg<'_>) -> anyhow::Result<()> {
        self.apply()?;
        let mut spans = vec![];
        Visitor {
            spans: &mut spans,
            cfg,
        }
        .visit_file(&self.file);
        return self.erase(
            |mask, _, _| {
                for span in spans {
                    set_span(mask, span, true);
                }
                Ok(())
            },
            || "broke the code during resolving `#[cfg(..)]`",
        );

        struct Visitor<'a> {
            spans: &'a mut Vec<Span>,
            cfg: &'a Cfg<'a>,
        }

        impl Visitor<'_> {
            fn proceed<'a, T: ToTokens>(
                &mut self,
                i: &'a T,
                attrs: fn(&T) -> &[Attribute],
                visit: fn(&mut Self, &'a T),
            ) {
                let attrs = attrs(i);

                let is_test = self.cfg.strip_tests
                    && attrs
                        .iter()
                        .any(|a| matches!(&a.meta, Meta::Path(path) if path.is_ident("test")));

                let sufficiencies = attrs
                    .iter()
                    .map(|a| (a.span(), &a.meta))
                    .flat_map(|(span, meta)| match meta {
                        Meta::List(meta_list) => Some((span, meta_list)),
                        _ => None,
                    })
                    .filter(|(_, MetaList { path, .. })| path.is_ident("cfg"))
                    .flat_map(|(span, meta_list)| {
                        let expr =
                            cfg_expr::Expression::parse(&meta_list.tokens.to_string()).ok()?;
                        Some((span, expr))
                    })
                    .map(|(span, expr)| {
                        let sufficiency = expr.eval(|pred| match pred {
                            cfg_expr::Predicate::Test => self.cfg.strip_tests.then_some(false),
                            cfg_expr::Predicate::ProcMacro => Some(false),
                            cfg_expr::Predicate::Flag(flag) if self.cfg.flags.contains(flag) => {
                                Some(true)
                            }
                            cfg_expr::Predicate::Feature(feature) => {
                                Some(self.cfg.features.contains(feature))
                            }
                            _ => None,
                        });
                        (span, sufficiency)
                    })
                    .collect::<Vec<_>>();

                if is_test || sufficiencies.iter().any(|&(_, p)| p == Some(false)) {
                    self.spans.push(i.span());
                } else {
                    for (span, p) in sufficiencies {
                        if p == Some(true) {
                            self.spans.push(span);
                        }
                    }
                    visit(self, i);
                }
            }
        }

        macro_rules! impl_visits {
            ($(fn $method:ident(&mut self, _: &'_ $ty:path) { _(_, _, $visit:path) })*) => {
                $(
                    fn $method(&mut self, i: &'_ $ty) {
                        self.proceed(i, |$ty { attrs, .. }| attrs, $visit);
                    }
                )*
            };
        }

        impl Visit<'_> for Visitor<'_> {
            impl_visits! {
                fn visit_arm                (&mut self, _: &'_ Arm              ) { _(_, _, visit::visit_arm                ) }
                fn visit_named_arg        (&mut self, _: &'_ NamedArg        ) { _(_, _, visit::visit_named_arg        ) }
                fn visit_const_param        (&mut self, _: &'_ ConstParam       ) { _(_, _, visit::visit_const_param        ) }
                fn visit_expr_array         (&mut self, _: &'_ ExprArray        ) { _(_, _, visit::visit_expr_array         ) }
                fn visit_expr_assign        (&mut self, _: &'_ ExprAssign       ) { _(_, _, visit::visit_expr_assign        ) }
                fn visit_expr_async         (&mut self, _: &'_ ExprAsync        ) { _(_, _, visit::visit_expr_async         ) }
                fn visit_expr_await         (&mut self, _: &'_ ExprAwait        ) { _(_, _, visit::visit_expr_await         ) }
                fn visit_expr_binary        (&mut self, _: &'_ ExprBinary       ) { _(_, _, visit::visit_expr_binary        ) }
                fn visit_expr_block         (&mut self, _: &'_ ExprBlock        ) { _(_, _, visit::visit_expr_block         ) }
                fn visit_expr_break         (&mut self, _: &'_ ExprBreak        ) { _(_, _, visit::visit_expr_break         ) }
                fn visit_expr_call          (&mut self, _: &'_ ExprCall         ) { _(_, _, visit::visit_expr_call          ) }
                fn visit_expr_cast          (&mut self, _: &'_ ExprCast         ) { _(_, _, visit::visit_expr_cast          ) }
                fn visit_expr_closure       (&mut self, _: &'_ ExprClosure      ) { _(_, _, visit::visit_expr_closure       ) }
                fn visit_expr_continue      (&mut self, _: &'_ ExprContinue     ) { _(_, _, visit::visit_expr_continue      ) }
                fn visit_expr_field         (&mut self, _: &'_ ExprField        ) { _(_, _, visit::visit_expr_field         ) }
                fn visit_expr_for_loop      (&mut self, _: &'_ ExprForLoop      ) { _(_, _, visit::visit_expr_for_loop      ) }
                fn visit_expr_group         (&mut self, _: &'_ ExprGroup        ) { _(_, _, visit::visit_expr_group         ) }
                fn visit_expr_if            (&mut self, _: &'_ ExprIf           ) { _(_, _, visit::visit_expr_if            ) }
                fn visit_expr_index         (&mut self, _: &'_ ExprIndex        ) { _(_, _, visit::visit_expr_index         ) }
                fn visit_expr_let           (&mut self, _: &'_ ExprLet          ) { _(_, _, visit::visit_expr_let           ) }
                fn visit_expr_lit           (&mut self, _: &'_ ExprLit          ) { _(_, _, visit::visit_expr_lit           ) }
                fn visit_expr_loop          (&mut self, _: &'_ ExprLoop         ) { _(_, _, visit::visit_expr_loop          ) }
                fn visit_expr_macro         (&mut self, _: &'_ ExprMacro        ) { _(_, _, visit::visit_expr_macro         ) }
                fn visit_expr_match         (&mut self, _: &'_ ExprMatch        ) { _(_, _, visit::visit_expr_match         ) }
                fn visit_expr_method_call   (&mut self, _: &'_ ExprMethodCall   ) { _(_, _, visit::visit_expr_method_call   ) }
                fn visit_expr_paren         (&mut self, _: &'_ ExprParen        ) { _(_, _, visit::visit_expr_paren         ) }
                fn visit_expr_path          (&mut self, _: &'_ ExprPath         ) { _(_, _, visit::visit_expr_path          ) }
                fn visit_expr_range         (&mut self, _: &'_ ExprRange        ) { _(_, _, visit::visit_expr_range         ) }
                fn visit_expr_reference     (&mut self, _: &'_ ExprReference    ) { _(_, _, visit::visit_expr_reference     ) }
                fn visit_expr_repeat        (&mut self, _: &'_ ExprRepeat       ) { _(_, _, visit::visit_expr_repeat        ) }
                fn visit_expr_return        (&mut self, _: &'_ ExprReturn       ) { _(_, _, visit::visit_expr_return        ) }
                fn visit_expr_struct        (&mut self, _: &'_ ExprStruct       ) { _(_, _, visit::visit_expr_struct        ) }
                fn visit_expr_try           (&mut self, _: &'_ ExprTry          ) { _(_, _, visit::visit_expr_try           ) }
                fn visit_expr_try_block     (&mut self, _: &'_ ExprTryBlock     ) { _(_, _, visit::visit_expr_try_block     ) }
                fn visit_expr_tuple         (&mut self, _: &'_ ExprTuple        ) { _(_, _, visit::visit_expr_tuple         ) }
                fn visit_expr_unary         (&mut self, _: &'_ ExprUnary        ) { _(_, _, visit::visit_expr_unary         ) }
                fn visit_expr_unsafe        (&mut self, _: &'_ ExprUnsafe       ) { _(_, _, visit::visit_expr_unsafe        ) }
                fn visit_expr_while         (&mut self, _: &'_ ExprWhile        ) { _(_, _, visit::visit_expr_while         ) }
                fn visit_expr_yield         (&mut self, _: &'_ ExprYield        ) { _(_, _, visit::visit_expr_yield         ) }
                fn visit_field              (&mut self, _: &'_ Field            ) { _(_, _, visit::visit_field              ) }
                fn visit_field_pat          (&mut self, _: &'_ FieldPat         ) { _(_, _, visit::visit_field_pat          ) }
                fn visit_field_value        (&mut self, _: &'_ FieldValue       ) { _(_, _, visit::visit_field_value        ) }
                fn visit_file               (&mut self, _: &'_ syn::File        ) { _(_, _, visit::visit_file               ) }
                fn visit_foreign_item_fn    (&mut self, _: &'_ ForeignItemFn    ) { _(_, _, visit::visit_foreign_item_fn    ) }
                fn visit_foreign_item_macro (&mut self, _: &'_ ForeignItemMacro ) { _(_, _, visit::visit_foreign_item_macro ) }
                fn visit_foreign_item_static(&mut self, _: &'_ ForeignItemStatic) { _(_, _, visit::visit_foreign_item_static) }
                fn visit_foreign_item_type  (&mut self, _: &'_ ForeignItemType  ) { _(_, _, visit::visit_foreign_item_type  ) }
                fn visit_impl_item_const    (&mut self, _: &'_ ImplItemConst    ) { _(_, _, visit::visit_impl_item_const    ) }
                fn visit_impl_item_macro    (&mut self, _: &'_ ImplItemMacro    ) { _(_, _, visit::visit_impl_item_macro    ) }
                fn visit_impl_item_fn   (&mut self, _: &'_ ImplItemFn   ) { _(_, _, visit::visit_impl_item_fn   ) }
                fn visit_impl_item_type     (&mut self, _: &'_ ImplItemType     ) { _(_, _, visit::visit_impl_item_type     ) }
                fn visit_item_const         (&mut self, _: &'_ ItemConst        ) { _(_, _, visit::visit_item_const         ) }
                fn visit_item_enum          (&mut self, _: &'_ ItemEnum         ) { _(_, _, visit::visit_item_enum          ) }
                fn visit_item_extern_crate  (&mut self, _: &'_ ItemExternCrate  ) { _(_, _, visit::visit_item_extern_crate  ) }
                fn visit_item_fn            (&mut self, _: &'_ ItemFn           ) { _(_, _, visit::visit_item_fn            ) }
                fn visit_item_foreign_mod   (&mut self, _: &'_ ItemForeignMod   ) { _(_, _, visit::visit_item_foreign_mod   ) }
                fn visit_item_impl          (&mut self, _: &'_ ItemImpl         ) { _(_, _, visit::visit_item_impl          ) }
                fn visit_item_macro         (&mut self, _: &'_ ItemMacro        ) { _(_, _, visit::visit_item_macro         ) }
                fn visit_item_mod           (&mut self, _: &'_ ItemMod          ) { _(_, _, visit::visit_item_mod           ) }
                fn visit_item_static        (&mut self, _: &'_ ItemStatic       ) { _(_, _, visit::visit_item_static        ) }
                fn visit_item_struct        (&mut self, _: &'_ ItemStruct       ) { _(_, _, visit::visit_item_struct        ) }
                fn visit_item_trait         (&mut self, _: &'_ ItemTrait        ) { _(_, _, visit::visit_item_trait         ) }
                fn visit_item_trait_alias   (&mut self, _: &'_ ItemTraitAlias   ) { _(_, _, visit::visit_item_trait_alias   ) }
                fn visit_item_type          (&mut self, _: &'_ ItemType         ) { _(_, _, visit::visit_item_type          ) }
                fn visit_item_union         (&mut self, _: &'_ ItemUnion        ) { _(_, _, visit::visit_item_union         ) }
                fn visit_item_use           (&mut self, _: &'_ ItemUse          ) { _(_, _, visit::visit_item_use           ) }
                fn visit_lifetime_param       (&mut self, _: &'_ LifetimeParam      ) { _(_, _, visit::visit_lifetime_param       ) }
                fn visit_local              (&mut self, _: &'_ Local            ) { _(_, _, visit::visit_local              ) }
                fn visit_pat_ident          (&mut self, _: &'_ PatIdent         ) { _(_, _, visit::visit_pat_ident          ) }
                fn visit_pat_or             (&mut self, _: &'_ PatOr            ) { _(_, _, visit::visit_pat_or             ) }
                fn visit_pat_reference      (&mut self, _: &'_ PatReference     ) { _(_, _, visit::visit_pat_reference      ) }
                fn visit_pat_rest           (&mut self, _: &'_ PatRest          ) { _(_, _, visit::visit_pat_rest           ) }
                fn visit_pat_slice          (&mut self, _: &'_ PatSlice         ) { _(_, _, visit::visit_pat_slice          ) }
                fn visit_pat_struct         (&mut self, _: &'_ PatStruct        ) { _(_, _, visit::visit_pat_struct         ) }
                fn visit_pat_tuple          (&mut self, _: &'_ PatTuple         ) { _(_, _, visit::visit_pat_tuple          ) }
                fn visit_pat_tuple_struct   (&mut self, _: &'_ PatTupleStruct   ) { _(_, _, visit::visit_pat_tuple_struct   ) }
                fn visit_pat_type           (&mut self, _: &'_ PatType          ) { _(_, _, visit::visit_pat_type           ) }
                fn visit_pat_wild           (&mut self, _: &'_ PatWild          ) { _(_, _, visit::visit_pat_wild           ) }
                fn visit_receiver           (&mut self, _: &'_ Receiver         ) { _(_, _, visit::visit_receiver           ) }
                fn visit_trait_item_const   (&mut self, _: &'_ TraitItemConst   ) { _(_, _, visit::visit_trait_item_const   ) }
                fn visit_trait_item_macro   (&mut self, _: &'_ TraitItemMacro   ) { _(_, _, visit::visit_trait_item_macro   ) }
                fn visit_trait_item_fn  (&mut self, _: &'_ TraitItemFn  ) { _(_, _, visit::visit_trait_item_fn  ) }
                fn visit_trait_item_type    (&mut self, _: &'_ TraitItemType    ) { _(_, _, visit::visit_trait_item_type    ) }
                fn visit_type_param         (&mut self, _: &'_ TypeParam        ) { _(_, _, visit::visit_type_param         ) }
                fn visit_fn_ptr_variadic    (&mut self, _: &'_ FnPtrVariadic    ) { _(_, _, visit::visit_fn_ptr_variadic    ) }
                fn visit_variadic           (&mut self, _: &'_ Variadic         ) { _(_, _, visit::visit_variadic           ) }
                fn visit_variant            (&mut self, _: &'_ Variant          ) { _(_, _, visit::visit_variant            ) }
            }
        }
    }

    /// Comments out `missing_docs` and `missing_crate_level_docs` inside
    /// `#[warn(..)]`, `#[deny(..)]` and `#[forbid(..)]`, so that removing the docs
    /// does not turn into a lint error.
    pub fn allow_missing_docs(&mut self) {
        Visitor {
            replacements: &mut self.replacements,
        }
        .visit_file(&self.file);

        struct Visitor<'a> {
            replacements: &'a mut std::collections::BTreeMap<
                (proc_macro2::LineColumn, proc_macro2::LineColumn),
                String,
            >,
        }

        impl Visit<'_> for Visitor<'_> {
            fn visit_attribute(&mut self, i: &Attribute) {
                if let Meta::List(meta_list) = &i.meta {
                    if ["warn", "deny", "forbid"]
                        .iter()
                        .any(|lint| meta_list.path.is_ident(lint))
                    {
                        for meta in nested_metas(meta_list) {
                            if let Meta::Path(path) = meta {
                                if ["missing_docs", "missing_crate_level_docs"]
                                    .iter()
                                    .any(|lint| path.is_ident(lint))
                                {
                                    let pos = path.span().start();
                                    self.replacements.insert((pos, pos), "/*".to_owned());
                                    let pos = path.span().end();
                                    self.replacements.insert((pos, pos), "*/".to_owned());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Removes doc comments (`///`, `//!`, `/** */`, `/*! */`) and `#[doc ..]`
    /// attributes, including ones inside macro bodies.
    pub fn erase_docs(&mut self) -> anyhow::Result<()> {
        return self.erase(
            |mask, _, token_stream| {
                visit_token_stream(mask, token_stream);
                Ok(())
            },
            || "broke the code during erasing doc comments",
        );

        // Walks the raw token stream rather than a `syn::File`, so that doc
        // comments inside macro bodies (`macro_rules!` definitions and macro
        // invocations) are reached too. `/// ..` and `//! ..` are lexed into
        // `#[doc = ".."]` / `#![doc = ".."]` token sequences.
        fn visit_token_stream(mask: &mut [FixedBitSet], token_stream: TokenStream) {
            let tts = token_stream.into_iter().collect::<Vec<_>>();
            let mut i = 0;
            while i < tts.len() {
                if let Some(len) = doc_attr_len(&tts[i..]) {
                    for tt in &tts[i..i + len] {
                        set_span(mask, tt.span(), true);
                    }
                    i += len;
                    continue;
                }
                if let TokenTree::Group(group) = &tts[i] {
                    visit_token_stream(mask, group.stream());
                }
                i += 1;
            }
        }

        /// Returns the number of leading token trees forming `#[doc ..]` or `#![doc ..]`.
        fn doc_attr_len(tts: &[TokenTree]) -> Option<usize> {
            let (rest, len) = match tts {
                [TokenTree::Punct(p), rest @ ..] if p.as_char() == '#' => (rest, 2),
                _ => return None,
            };
            let (rest, len) = match rest {
                [TokenTree::Punct(p), rest @ ..] if p.as_char() == '!' => (rest, len + 1),
                _ => (rest, len),
            };
            match rest {
                [TokenTree::Group(g), ..] if g.delimiter() == Delimiter::Bracket => {
                    match g.stream().into_iter().next() {
                        Some(TokenTree::Ident(i)) if i == "doc" => Some(len),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
    }

    /// Removes `// ..` and `/* .. */` comments. Doc comments are tokens, not
    /// comments, so they stay; use [`erase_docs`](Self::erase_docs) for those.
    pub fn erase_comments(&mut self) -> anyhow::Result<()> {
        return self.erase(
            |mask, lines, token_stream| {
                for mask in &mut *mask {
                    mask.insert_range(..);
                }
                visit_token_stream(mask, token_stream);
                // What is left marked is comments and whitespace. The whitespace
                // can stay: a comment blanked out is whitespace too, and leaving
                // it unmarked means a file with no comments is not touched.
                for (mask, line) in mask.iter_mut().zip(lines) {
                    for (j, c) in line.chars().enumerate() {
                        if c.is_whitespace() {
                            mask.set(j, false);
                        }
                    }
                }
                Ok(())
            },
            || "broke the code during erasing comments",
        );

        fn visit_token_stream(mask: &mut [FixedBitSet], token_stream: TokenStream) {
            for tt in token_stream {
                if let TokenTree::Group(group) = tt {
                    set_span(mask, group.span_open(), false);
                    visit_token_stream(mask, group.stream());
                    set_span(mask, group.span_close(), false);
                } else {
                    set_span(mask, tt.span(), false);
                }
            }
        }
    }

    /// Blanks the characters `select` marks, then tidies.
    ///
    /// `select` gets one bit set per line, sized to the line, the lines themselves
    /// and the source as a token stream. Marked characters become spaces, which keeps every token that
    /// stays from running into its neighbour. A line that had content and is left
    /// with only whitespace is dropped, and the trailing whitespace of a line that
    /// changed is trimmed. Neither is done where the end of a line falls inside a
    /// string literal that is kept, since there the whitespace belongs to the
    /// string. A blank line next to a dropped one is dropped too, so a removal does
    /// not leave a double gap.
    ///
    /// When nothing is marked, the text is left exactly as it was.
    fn erase(
        &mut self,
        select: impl FnOnce(&mut [FixedBitSet], &[&str], TokenStream) -> syn::Result<()>,
        err_msg: fn() -> &'static str,
    ) -> anyhow::Result<()> {
        self.apply()?;

        let code = if self.string.contains("\r\n") {
            Cow::from(self.string.replace("\r\n", "\n"))
        } else {
            Cow::from(&self.string)
        };

        // A shebang line is not Rust tokens. Blank it for tokenizing, which keeps
        // every later position where it is, and emit it as it was. `#!` followed by
        // `[` opens an inner attribute instead, which is ordinary code.
        let first_line = &code[..code.find('\n').unwrap_or(code.len())];
        let has_shebang = code.starts_with("#!") && !code[2..].trim_start().starts_with('[');
        let tokenizable = if has_shebang {
            Cow::from(format!(
                "{}{}",
                " ".repeat(first_line.chars().count()),
                &code[first_line.len()..],
            ))
        } else {
            Cow::from(&*code)
        };

        let token_stream = tokenizable
            .parse::<TokenStream>()
            .map_err(|e| anyhow!("{}", e))
            .with_context(|| "broke the code during modification")?;

        let lines = tokenizable.lines().collect::<Vec<_>>();
        let mut mask = lines
            .iter()
            .map(|l| FixedBitSet::with_capacity(l.chars().count()))
            .collect::<Vec<_>>();

        select(&mut mask, &lines, token_stream.clone())
            .map_err(|e| anyhow!("{:?}", e))
            .with_context(err_msg)?;

        if has_shebang {
            mask[0].clear();
        }

        if mask.iter().all(|m| m.count_ones(..) == 0) {
            return Ok(());
        }

        let protected = lines_ending_inside_kept_literals(token_stream, &mask);

        let mut acc = "".to_owned();
        let mut last_blank = true;
        let mut dropped = false;
        for (i, (line, mask)) in code.lines().zip_eq(&mask).enumerate() {
            let changed = mask.count_ones(..) > 0;
            let text = if changed {
                Cow::from(
                    line.chars()
                        .enumerate()
                        .map(|(j, c)| if mask[j] { ' ' } else { c })
                        .collect::<String>(),
                )
            } else {
                Cow::from(line)
            };

            if protected.contains(&i) {
                acc += &text;
                acc += "\n";
                last_blank = false;
                dropped = false;
                continue;
            }

            let text = if changed { text.trim_end() } else { &*text };
            if text.trim().is_empty() {
                if changed && !line.trim().is_empty() {
                    dropped = true;
                    continue;
                }
                if last_blank && dropped {
                    continue;
                }
                acc += "\n";
                last_blank = true;
                dropped = false;
                continue;
            }
            acc += text;
            acc += "\n";
            last_blank = false;
            dropped = false;
        }

        let acc = acc.trim_start();
        let acc = acc.trim_end_matches('\n');
        self.string = if acc.is_empty() {
            "".to_owned()
        } else {
            format!("{}\n", acc)
        };
        self.force_apply()
    }
}

/// Lines (0-based) whose end falls inside a multi-line literal that survives the
/// mask, and whose whitespace therefore belongs to the literal.
fn lines_ending_inside_kept_literals(
    token_stream: TokenStream,
    mask: &[FixedBitSet],
) -> BTreeSet<usize> {
    let mut acc = BTreeSet::new();
    visit(token_stream, mask, &mut acc);
    return acc;

    fn visit(token_stream: TokenStream, mask: &[FixedBitSet], acc: &mut BTreeSet<usize>) {
        for tt in token_stream {
            match tt {
                TokenTree::Group(group) => visit(group.stream(), mask, acc),
                TokenTree::Literal(literal) => {
                    let (start, end) = (literal.span().start(), literal.span().end());
                    let kept = !mask[start.line - 1].contains(start.column);
                    if start.line < end.line && kept {
                        acc.extend(start.line - 1..end.line - 1);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Options, strip};
    use pretty_assertions::assert_eq;

    fn run(input: &str, options: Options) -> String {
        strip(input, &options).unwrap()
    }

    fn only(f: impl FnOnce(&mut Options)) -> Options {
        let mut options = Options {
            strip_tests: false,
            strip_docs: false,
            strip_comments: false,
            features: vec![],
            minify: false,
        };
        f(&mut options);
        options
    }

    #[test]
    fn tests_are_removed() {
        let input = r#"fn f() {}

#[cfg(test)]
mod tests {
    #[test]
    fn t() {}
}

#[test]
fn bare() {}

#[my::test]
fn not_std_test() {}
"#;
        assert_eq!(
            "fn f() {}\n\n#[my::test]\nfn not_std_test() {}\n",
            run(input, only(|o| o.strip_tests = true)),
        );
        assert_eq!(input, run(input, only(|_| ())));
    }

    #[test]
    fn cfgs() {
        let input = r#"#[cfg(not(test))]
fn not_test() {}

#[cfg(feature = "on")]
fn on() {}

#[cfg(feature = "off")]
fn off() {}

#[cfg(target_os = "linux")]
fn unknown() {}
"#;
        assert_eq!(
            r#"fn not_test() {}

fn on() {}

#[cfg(target_os = "linux")]
fn unknown() {}
"#,
            run(
                input,
                only(|o| {
                    o.strip_tests = true;
                    o.features = vec!["on".to_owned()];
                }),
            ),
        );
        // `cfg(test)` is undecided when tests are kept, so `not(test)` is too.
        assert!(run(input, only(|_| ())).starts_with("#[cfg(not(test))]\nfn not_test() {}\n"));
    }

    #[test]
    fn docs() {
        let input = r#"//! crate
/// item
fn a() {}

/** block */
#[doc = "attr"]
#[doc(hidden)]
fn b() {}

macro_rules! m {
    () => {
        /// inside a macro body
        struct S;
    };
}
"#;
        assert_eq!(
            r#"fn a() {}

fn b() {}

macro_rules! m {
    () => {
        struct S;
    };
}
"#,
            run(input, only(|o| o.strip_docs = true)),
        );
    }

    #[test]
    fn missing_docs_lint_is_disarmed() {
        let input = "#![deny(missing_docs)]\n//! crate\n";
        assert_eq!(
            "#![deny(/*missing_docs*/)]\n",
            run(input, only(|o| o.strip_docs = true)),
        );
    }

    #[test]
    fn comments() {
        let input = r#"// line
fn f() {
    let a = 1; // trailing
    /* block /* nested */ still block */
    let b = a/**/+1;
}
"#;
        assert_eq!(
            // The comment between `a` and `+1` becomes spaces, so the tokens do
            // not run together.
            "fn f() {\n    let a = 1;\n    let b = a    +1;\n}\n",
            run(input, only(|o| o.strip_comments = true)),
        );
    }

    /// The `erase` this was ported from took any file starting with `#!` for one
    /// with a shebang, and dropped its first line. A file starting with an inner
    /// attribute lost that attribute.
    #[test]
    fn inner_attribute_on_the_first_line_is_kept() {
        let input = "#![allow(dead_code)]\n/// doc\nfn f() {}\n";
        assert_eq!(
            "#![allow(dead_code)]\nfn f() {}\n",
            run(input, only(|o| o.strip_docs = true)),
        );
    }

    #[test]
    fn shebang_is_kept() {
        let input = "#!/usr/bin/env run-cargo-script\n// c\nfn main() {}\n";
        assert_eq!(
            "#!/usr/bin/env run-cargo-script\nfn main() {}\n",
            run(input, only(|o| o.strip_comments = true)),
        );
    }

    /// The `set_span` this was ported from set the bits on the first line of a
    /// multi-line span even when asked to clear them, so removing comments blanked
    /// out the first line of a multi-line string literal, opening quote included.
    #[test]
    fn multi_line_literals_are_untouched() {
        let input = "fn f() -> &'static str {\n    \"first  \n\n  // not a comment   \n/* nor this */\" // but this is\n}\n\nconst R: &str = r#\"/*\n*/\"#;\n";
        assert_eq!(
            "fn f() -> &'static str {\n    \"first  \n\n  // not a comment   \n/* nor this */\"\n}\n\nconst R: &str = r#\"/*\n*/\"#;\n",
            run(input, only(|o| o.strip_comments = true)),
        );
    }

    #[test]
    fn unchanged_input_is_returned_as_is() {
        let input = "fn main() {\r\n    println!(\"hi\");\r\n}\r\n";
        assert_eq!(input, run(input, Options::default()));
    }

    #[test]
    fn stripping_is_idempotent() {
        let input = r#"//! c
/// d
fn f() -> i32 {
    // e
    1 /* f */
}

#[cfg(test)]
mod tests {}
"#;
        let once = run(input, Options::default());
        assert_eq!(once, run(&once, Options::default()));
    }

    #[test]
    fn minify() {
        let input = "/// d\nfn f() -> i32 {\n    // e\n    1 + 2\n}\n";
        assert_eq!(
            "fn f()->i32{1+2}\n",
            run(
                input,
                Options {
                    minify: true,
                    ..Options::default()
                }
            ),
        );
    }
}
