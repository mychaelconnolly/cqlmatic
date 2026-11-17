use cqlmatic::setup::*;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-skill-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn cqlmatic(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("COPILOT_HOME");
    command
}

/// The skill instruction a completed setup gives for this client.
fn hint(client: ClientKind) -> String {
    let actions = result(
        SetupState {
            phase: SetupPhase::ToolDiscoveryVerified,
            selection: Some(Selection {
                client,
                host_id: (client == ClientKind::External).then(|| "synthetic-host".into()),
                cloud: Cloud::Us2,
                server_name: "cs-mcp".into(),
                registration: RegistrationMode::HostManaged {},
            }),
            ..SetupState::default()
        },
        None,
    )
    .next_actions;
    actions
        .into_iter()
        .find(|action| action.contains("cqlmatic skill install"))
        .unwrap_or_else(|| panic!("no skill instruction for {client:?}"))
}

#[test]
fn completed_setup_names_an_install_command_that_works() {
    for (client, directory) in [
        (ClientKind::ClaudeCode, ".claude/skills"),
        (ClientKind::Codex, ".agents/skills"),
        (ClientKind::VsCode, ".agents/skills"),
        (ClientKind::CopilotCli, ".agents/skills"),
        (ClientKind::CopilotApp, ".agents/skills"),
        (ClientKind::CopilotVscode, ".agents/skills"),
    ] {
        let hint = hint(client);
        let arguments = hint
            .split_once("run cqlmatic ")
            .unwrap_or_else(|| panic!("{hint}"))
            .1
            .trim_end_matches('.');
        let home = Temp::new();
        let output = cqlmatic(&home.0)
            .args(arguments.split(' '))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{arguments}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let skill = home.0.join(directory).join("cqlmatic-companion");
        assert!(
            fs::read_to_string(skill.join("SKILL.md"))
                .unwrap()
                .starts_with("---\nname: cqlmatic-companion\n"),
            "{client:?}"
        );
        assert!(skill.join("references/MCP-COMPANION.md").is_file());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("7 created, 0 updated, 0 unchanged.")
        );
    }
    let hint = hint(ClientKind::External);
    assert!(hint.contains("cqlmatic skill install --dir"), "{hint}");
}

#[test]
fn install_honours_relocated_client_homes_and_an_explicit_directory() {
    let home = Temp::new();
    for (variable, client, expected) in [
        ("CLAUDE_CONFIG_DIR", "claude-code", "claude-home/skills"),
        ("COPILOT_HOME", "copilot-cli", "copilot-home/skills"),
    ] {
        let relocated = home.0.join(expected).parent().unwrap().to_path_buf();
        let output = cqlmatic(&home.0)
            .env(variable, &relocated)
            .args(["skill", "install", "--client", client])
            .output()
            .unwrap();
        assert!(output.status.success(), "{client}");
        assert!(
            home.0
                .join(expected)
                .join("cqlmatic-companion/SKILL.md")
                .is_file()
        );
    }
    assert!(!home.0.join(".claude").exists());
    assert!(!home.0.join(".agents").exists());
    let explicit = home.0.join("other host/skills");
    let output = cqlmatic(&home.0)
        .args(["skill", "install", "--dir"])
        .arg(&explicit)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(explicit.join("cqlmatic-companion/SKILL.md").is_file());
}

#[test]
fn install_requires_one_target_and_reports_refusals_as_diagnostics() {
    let home = Temp::new();
    for arguments in [
        &["skill", "install"][..],
        &["skill", "install", "--client", "codex", "--dir", "x"][..],
        &["skill", "install", "--client", "external"][..],
    ] {
        let output = cqlmatic(&home.0).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
    }
    assert_eq!(fs::read_dir(&home.0).unwrap().count(), 0);
    fs::write(home.0.join("cqlmatic-companion"), b"a file").unwrap();
    let output = cqlmatic(&home.0)
        .args(["skill", "install", "--dir"])
        .arg(&home.0)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "SKILL_TARGET");
}
