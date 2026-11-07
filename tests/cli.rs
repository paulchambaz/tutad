use std::process::Command;

fn tutad(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tutad"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn version_prints_the_crate_version() {
    let output = tutad(&["-V"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("tutad {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_prints_the_usage() {
    let output = tutad(&["-h"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("Usage: tutad"));
}

#[test]
fn an_unknown_command_exits_with_2() {
    let output = tutad(&["fly"]);
    assert_eq!(output.status.code(), Some(2));
}
