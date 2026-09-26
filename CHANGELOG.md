# Changelog

## [0.1.0] - Unreleased

First release.

- `rs-strip [FILE]` removes tests, doc comments and comments from one Rust source
  file, and `--minify` prints the result on one line.
- Removal is done on the text, so what stays keeps its layout, and it reaches
  inside `macro_rules!` bodies.
- The library exposes `strip` and the building blocks it is made of.
