//! Tests for shell completion and man page generation (spec 007 §Open Q3).

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn bash_completions_contain_subcommands() {
    Command::cargo_bin("infmonctl")
        .unwrap()
        .args(["--generate-completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_infmonctl"))
        .stdout(predicate::str::contains("install"))
        .stdout(predicate::str::contains("flow-rule"));
}

#[test]
fn zsh_completions_contain_compdef() {
    Command::cargo_bin("infmonctl")
        .unwrap()
        .args(["--generate-completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef infmonctl"))
        .stdout(predicate::str::contains("install"));
}

#[test]
fn fish_completions_generated() {
    Command::cargo_bin("infmonctl")
        .unwrap()
        .args(["--generate-completions", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("complete -c infmonctl"));
}

#[test]
fn unsupported_shell_exits_2() {
    Command::cargo_bin("infmonctl")
        .unwrap()
        .args(["--generate-completions", "powershell"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unsupported shell"));
}

#[test]
fn manpage_contains_name_section() {
    Command::cargo_bin("infmonctl")
        .unwrap()
        .arg("--generate-manpage")
        .assert()
        .success()
        .stdout(predicate::str::contains(".TH infmonctl"))
        .stdout(predicate::str::contains(".SH NAME"));
}

#[test]
fn generate_completions_equals_syntax() {
    Command::cargo_bin("infmonctl")
        .unwrap()
        .arg("--generate-completions=bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("_infmonctl"));
}
