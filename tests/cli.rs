//! Runs the `rs-strip` binary the way a user would.

use std::{
    fs,
    io::Write as _,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

const INPUT: &str = r#"//! crate docs
/// Adds one.
fn inc(x: i64) -> i64 {
    x + 1 // comment
}

#[cfg(feature = "local")]
fn debug() {}

fn main() {
    println!("{}", inc(1));
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        assert_eq!(super::inc(1), 2);
    }
}
"#;

const STRIPPED: &str = r#"fn inc(x: i64) -> i64 {
    x + 1
}

fn main() {
    println!("{}", inc(1));
}
"#;

fn rs_strip(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rs-strip"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stdout(output: &Output) -> &str {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr),
    );
    std::str::from_utf8(&output.stdout).unwrap()
}

fn tmp(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

#[test]
fn reads_stdin_by_default() {
    assert_eq!(STRIPPED, stdout(&rs_strip(&[], INPUT)));
    assert_eq!(STRIPPED, stdout(&rs_strip(&["-"], INPUT)));
}

#[test]
fn reads_a_file_and_writes_a_file() {
    let (input, output) = (tmp("in.rs"), tmp("out.rs"));
    fs::write(&input, INPUT).unwrap();
    let run = rs_strip(
        &[input.to_str().unwrap(), "-o", output.to_str().unwrap()],
        "",
    );
    assert_eq!("", stdout(&run));
    assert_eq!(STRIPPED, fs::read_to_string(output).unwrap());
}

#[test]
fn keeps_what_it_is_told_to() {
    let out = rs_strip(&["--keep-tests", "--keep-docs", "--keep-comments"], INPUT);
    let out = stdout(&out);
    assert!(out.contains("#[cfg(test)]"));
    assert!(out.contains("/// Adds one."));
    assert!(out.contains("// comment"));
    // Features are resolved either way.
    assert!(!out.contains("fn debug"));
}

#[test]
fn features_keep_their_items() {
    let out = rs_strip(&["--features", "local,other"], INPUT);
    assert!(stdout(&out).contains("fn debug() {}"));
}

#[test]
fn minify() {
    assert_eq!(
        "fn inc(x:i64)->i64{x+1}fn main(){println!(\"{}\",inc(1));}\n",
        stdout(&rs_strip(&["--minify"], INPUT)),
    );
}

#[test]
fn invalid_input_is_an_error() {
    let out = rs_strip(&[], "fn main( {");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("could not parse the input"));
}

#[test]
fn a_missing_file_is_an_error() {
    let out = rs_strip(&["no/such/file.rs"], "");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no/such/file.rs"));
}
