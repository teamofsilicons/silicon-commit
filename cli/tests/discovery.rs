//! The three discovery commands Silicon Apps runs in a clean environment, and the help tree.

mod support;

use serde_json::json;
use std::fs;
use support::{Home, json_output};

#[test]
fn discovery_commands_answer_signed_out_in_an_empty_home() {
    let home = Home::new();
    let help = home.command().arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: commit"));

    let accounts = json_output(
        home.command()
            .args(["accounts", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        accounts,
        json!({
            "app_id": "commit",
            "accounts_url": "https://accounts.teamofsilicons.com",
            "api_url": "https://api.commit.teamofsilicons.com",
            "version": env!("CARGO_PKG_VERSION"),
            "command": "commit",
            "profile": "default",
            "login": {
                "carbon": "commit login",
                "silicon": "silicon-accounts login --app commit -q | commit login --slt-stdin",
                "status": "commit login status --json"
            },
            "docs": "https://docs.commit.teamofsilicons.com",
            "repository": "https://github.com/teamofsilicons/silicon-commit",
            "rust_client": "https://crates.io/crates/silicon-commit-client"
        })
    );
    // The hidden transition alias prints exactly the same object.
    assert_eq!(
        json_output(home.command().args(["iam", "--json"]).output().unwrap()),
        accounts
    );
    assert_eq!(
        json_output(home.command().arg("accounts").output().unwrap()),
        accounts
    );

    let status = home
        .command()
        .args(["login", "status", "--json"])
        .output()
        .unwrap();
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&status.stdout).trim(),
        "{\n  \"authenticated\": false\n}"
    );

    let text = home.command().args(["login", "status"]).output().unwrap();
    assert_eq!(
        text.status.code(),
        Some(1),
        "text status exits 1 when signed out"
    );
    assert!(String::from_utf8_lossy(&text.stdout).contains("Not signed in"));

    assert!(!home.state().exists(), "discovery never writes state");
}

#[test]
fn discovery_never_fails_on_odd_configuration_or_damaged_state() {
    let home = Home::new();
    fs::create_dir_all(home.state()).unwrap();
    fs::write(home.state().join("session.json"), "not json at all").unwrap();
    let accounts = json_output(
        home.command()
            .env("COMMIT_API_URL", "invalid-url")
            .env("ACCOUNTS_URL", "http://localhost:9590/")
            .env("COMMIT_PROFILE", "default")
            .args(["accounts", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(accounts["api_url"], "invalid-url");
    assert_eq!(accounts["accounts_url"], "http://localhost:9590");
    let status = json_output(
        home.command()
            .args(["login", "status", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "unreadable_session");
    // A session path that cannot even be read is still an answer with --json.
    fs::remove_file(home.state().join("session.json")).unwrap();
    fs::create_dir_all(home.state().join("session.json")).unwrap();
    let status = home
        .command()
        .args(["login", "status", "--json"])
        .output()
        .unwrap();
    assert_eq!(status.status.code(), Some(0));
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "io_error");
    let text = home.command().args(["login", "status"]).output().unwrap();
    assert_eq!(text.status.code(), Some(1));
    let token = support::jwt("zQo", "c:ada", "carbon", support::now() + 900);
    let unverified = json_output(
        home.command()
            .env("COMMIT_API_URL", "invalid-url")
            .args(["--token", &token, "login", "status", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(unverified["authenticated"], true);
    assert_eq!(unverified["verified"], false);
    assert!(
        unverified["warning"]
            .as_str()
            .unwrap()
            .contains("invalid-url")
    );
}

#[test]
fn the_help_tree_documents_every_command_without_retired_concepts() {
    let home = Home::new();
    let pages: &[&[&str]] = &[
        &["--help"],
        &["-h"],
        &["login", "--help"],
        &["login", "status", "--help"],
        &["logout", "--help"],
        &["accounts", "--help"],
        &["me", "--help"],
        &["todos", "--help"],
        &["todos", "list", "--help"],
        &["todos", "create", "--help"],
        &["todos", "update", "--help"],
        &["todos", "set-subscription", "--help"],
        &["projects", "--help"],
        &["projects", "create", "--help"],
        &["projects", "set-diary", "--help"],
        &["notifications", "--help"],
        &["email", "--help"],
        &["silicons", "--help"],
        &["silicons", "allow", "--help"],
        &["report", "--help"],
        &["config", "--help"],
        &["config", "home", "--help"],
        &["docs", "--help"],
        &["health", "--help"],
    ];
    for args in pages {
        let output = home
            .command()
            .env("COMMIT_ACCESS_TOKEN", "eyJ.private.help-token")
            .args(*args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("Usage:"), "{args:?}: {text}");
        assert!(
            !text.contains("eyJ.private.help-token"),
            "{args:?} leaks the token"
        );
        for retired in [
            "IAM",
            "Honeycomb",
            "honeycomb",
            "organization",
            "organisation",
            "--org",
            "org_id",
            "--test",
            "sandbox",
            "testing environment",
            "self-update",
            "daemon",
            " iam ",
            "AI agent",
            "human",
        ] {
            assert!(
                !text.contains(retired),
                "{args:?} mentions `{retired}`:\n{text}"
            );
        }
    }
    let create = home
        .command()
        .args(["todos", "create", "--help"])
        .output()
        .unwrap();
    let create = String::from_utf8_lossy(&create.stdout);
    for part in [
        "assigned_to",
        "c:alice",
        "si:scribe",
        "silicon_not_reachable",
    ] {
        assert!(create.contains(part), "todos create help lacks {part}");
    }
    let root = home.command().arg("--help").output().unwrap();
    let root = String::from_utf8_lossy(&root.stdout);
    for part in [
        "commit login",
        "silicon-accounts login --app commit -q | commit login --slt-stdin",
        "commit accounts --json",
        "commit report",
    ] {
        assert!(root.contains(part), "root help lacks {part}");
    }
}

#[test]
fn retired_commands_and_flags_are_gone() {
    let home = Home::new();
    for args in [
        vec!["--org-id", "tos", "todos", "list"],
        vec!["--test", "secret", "todos", "list"],
        vec!["--no-update", "todos", "list"],
        vec!["testing", "status"],
        vec!["test-environments", "list"],
        vec!["daemon", "status"],
        vec!["config", "updates", "off"],
    ] {
        let output = home.command().args(&args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?} should be a usage error"
        );
    }
}

#[test]
fn bundled_guides_print_offline() {
    let home = Home::new();
    for topic in [
        "start",
        "usage",
        "cli",
        "projects",
        "notifications",
        "client",
        "api",
        "accounts",
        "contracts",
        "development",
        "telemetry",
        "deployment",
    ] {
        let output = home.command().args(["docs", topic]).output().unwrap();
        assert!(output.status.success(), "{topic}");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.starts_with("# "), "{topic}");
        assert!(text.contains("https://docs.commit.teamofsilicons.com"));
    }
    let unknown = home.command().args(["docs", "testing"]).output().unwrap();
    assert_eq!(unknown.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("start, cli, projects"));
}
