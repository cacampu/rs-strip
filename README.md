# rs-strip

Strip a single Rust source file down for submission to a competitive programming
judge: remove tests, doc comments and comments, and optionally minify.

```console
$ rs-strip src/main.rs | xsel -b
```

```rust
//! Solution.
use std::io::Read;

/// Adds two numbers.
fn add(a: i64, b: i64) -> i64 {
    // the answer
    a + b
}

fn main() {
    println!("{}", add(1, 2));
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        assert_eq!(super::add(1, 2), 3);
    }
}
```

↓

```rust
use std::io::Read;

fn add(a: i64, b: i64) -> i64 {
    a + b
}

fn main() {
    println!("{}", add(1, 2));
}
```

It works on the text of one file and needs no Cargo project, which also makes it a
good last step after a bundler.

## Installation

```console
$ cargo install rs-strip
```

Or the latest from Git:

```console
$ cargo install --git https://github.com/cacampu/rs-strip
```

## Usage

```console
$ rs-strip [OPTIONS] [FILE]
```

Reads `FILE`, or stdin when it is omitted or `-`, and writes to stdout.

| Option | |
|---|---|
| `-o, --output <PATH>` | Write to a file instead of stdout |
| `-m, --minify` | Also print the result on one line with as little whitespace as still parses |
| `--keep-tests` | Keep `#[cfg(test)]` items and `#[test]` functions |
| `--keep-docs` | Keep doc comments |
| `--keep-comments` | Keep comments |
| `--features <FEATURES>` | Features to treat as enabled, comma-separated |

## What is removed

- **Tests.** Items whose `#[cfg(..)]` is false once `test` is false, and functions
  marked `#[test]`, which the compiler drops outside a test build anyway.
- **Items behind features that are not enabled.** A judge compiles a single file
  with no features, so `#[cfg(feature = "..")]` is false unless the feature is
  passed with `--features`. When a `#[cfg(..)]` is true, the attribute is removed
  and the item stays.
- **Doc comments.** `///`, `//!`, `/** */`, `/*! */` and `#[doc ..]`. A
  `#![deny(missing_docs)]` is disarmed so that the result still compiles.
- **Comments.** `//` and `/* */`, nested ones included.

`cfg` predicates it cannot decide, such as `target_os` or `debug_assertions`, are
left alone together with their items.

## How it differs from printing the syntax tree back out

Many tools parse the file and print the tree again. That throws away the layout,
and it cannot see into macro bodies, since those are raw tokens to a parser.
rs-strip instead finds what to remove on the syntax tree and the token stream, and
blanks it out in the original text:

- Everything that stays keeps its layout. Lines emptied by a removal are dropped,
  and nothing else moves.
- Doc comments inside `macro_rules!` bodies and macro invocations are removed too.
- String literals are never touched, including ones spanning several lines with
  `//` or trailing whitespace inside.
- A file with nothing to remove comes back byte for byte.

## As a library

```rust
let stripped = rs_strip::strip(code, &rs_strip::Options::default())?;
```

The building blocks are public for tools that layer their own transformations on
top: `SourceEdit` with `resolve_cfgs`, `erase_docs` and `erase_comments`, and
`minify_tokens`. Turn off the default `cli` feature to leave out clap:

```toml
rs-strip = { version = "0.1", default-features = false }
```

## Credits

rs-strip grew out of [cargo-equip] and [rustminify], both by Ryo Yamashita
([@qryxip]). The removal and minification code started from theirs.

[cargo-equip]: https://github.com/qryxip/cargo-equip
[rustminify]: https://github.com/qryxip/rustminify
[@qryxip]: https://github.com/qryxip

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
