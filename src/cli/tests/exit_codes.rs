//! Tests for infmonctl exit codes (spec 007 §Exit codes).
//!
//! These are table-driven tests that assert the documented exit codes
//! for various error branches.

use assert_cmd::Command;
use predicates::prelude::*;

fn cmd() -> Command {
    Command::cargo_bin("infmonctl").expect("binary must exist")
}

// ---------------------------------------------------------------------------
// EXIT_USAGE (2): bad flags, bad arguments
// ---------------------------------------------------------------------------

#[test]
fn exit_usage_on_unknown_subcommand() {
    cmd().arg("nonexistent").assert().failure().code(2);
}

#[test]
fn exit_usage_on_missing_required_arg() {
    // stats export requires --format
    cmd()
        .args(["stats", "export"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn exit_usage_on_flow_show_missing_key() {
    // flow show requires both <rule> and <key>
    cmd()
        .args(["flow", "show", "rule1"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn exit_usage_on_config_get_missing_key() {
    cmd().args(["config", "get"]).assert().failure().code(2);
}

#[test]
fn exit_usage_on_config_set_missing_value() {
    cmd()
        .args(["config", "set", "key"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn exit_usage_on_flow_rule_add_missing_spec() {
    cmd()
        .args(["flow-rule", "add"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn exit_usage_on_flow_rule_rm_missing_target() {
    cmd()
        .args(["flow-rule", "rm"])
        .assert()
        .failure()
        .code(2);
}

// ---------------------------------------------------------------------------
// EXIT_SUCCESS (0): --help, --version
// ---------------------------------------------------------------------------

#[test]
fn exit_success_on_help() {
    cmd().arg("--help").assert().success();
}

#[test]
fn exit_success_on_version() {
    cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("infmonctl"));
}

#[test]
fn exit_success_on_subcommand_help() {
    cmd().args(["config", "--help"]).assert().success();
}
