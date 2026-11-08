use cqlmatic::{setup::*, sha256_hex};
use serde_json::json;

fn selection(client: ClientKind) -> Selection {
    Selection {
        client,
        cloud: Cloud::Us2,
        server_name: "cs-mcp".into(),
        registration: RegistrationMode::Cimd {
            metadata_url: match client {
                ClientKind::Codex => "https://chatgpt.com/oauth/codex/synthetic/client.json",
                ClientKind::VsCode => "https://vscode.dev/oauth/client-metadata.json",
            }
            .into(),
            allowlist_confirmed: true,
        },
    }
}
fn observation(client: ClientKind) -> ClientObservation {
    ClientObservation {
        client,
        executable: std::env::temp_dir()
            .join("synthetic-native-client.exe")
            .to_string_lossy()
            .into_owned(),
        executable_sha256: sha256_hex(b"synthetic executable"),
        version: "synthetic 1.0".into(),
        supports_http: true,
        supports_cimd: true,
        supports_manual: true,
        supports_login: true,
        capability_sha256: sha256_hex(b"synthetic capability"),
    }
}
fn selected(client: ClientKind) -> SetupState {
    let mut state = SetupState {
        clients: vec![observation(client)],
        ..SetupState::default()
    };
    state.select(selection(client)).unwrap();
    state
}
fn baseline(server: ExistingServer) -> ConfigBaseline {
    ConfigBaseline {
        path: std::env::temp_dir()
            .join("synthetic-config.json")
            .to_string_lossy()
            .into_owned(),
        sha256: None,
        server,
    }
}
#[test]
fn strict_requests_reject_unknown_secret_and_executable_fields() {
    for value in [
        json!({"schema_version":1,"action":{"type":"detect","executable":"arbitrary.exe"}}),
        json!({"schema_version":1,"secret":"synthetic","action":{"type":"status"}}),
        json!({"schema_version":1,"action":{"type":"select","selection":{"client":"codex","cloud":"us1","server_name":"cs-mcp","registration":{"mode":"manual","client_id":"synthetic","callback_url":"http://127.0.0.1:1234/callback","client_secret":"synthetic"}}}}),
    ] {
        assert!(serde_json::from_value::<SetupRequest>(value).is_err());
    }
    let request: SetupRequest =
        serde_json::from_value(json!({"schema_version":2,"action":{"type":"status"}})).unwrap();
    assert_eq!(request.validate().unwrap_err().code, "SETUP_VERSION");
}
#[test]
fn bundled_setup_examples_match_strict_contracts() {
    for text in [
        include_str!("../examples/setup/detect.request.json"),
        include_str!("../examples/setup/select.request.json"),
        include_str!("../examples/setup/preview.request.json"),
    ] {
        serde_json::from_str::<SetupRequest>(text)
            .unwrap()
            .validate()
            .unwrap();
    }
    let report: HostToolDiscovery =
        serde_json::from_str(include_str!("../examples/setup/tool-discovery.json")).unwrap();
    assert_eq!(report.tools[0].name, SEARCH_TOOL);
}
#[test]
fn fresh_state_has_no_environment_or_authentication_claim() {
    let state = SetupState::default();
    state.validate().unwrap();
    assert_eq!(state.phase, SetupPhase::New);
    assert!(!state.configured);
    assert!(state.selection.is_none());
    assert_eq!(state.authentication_evidence, AuthenticationEvidence::None);
    assert!(result(state, None).handoff.is_none());
}
#[test]
fn endpoints_are_canonical_per_cloud() {
    assert_eq!(Cloud::Us1.endpoint(), "https://mcp.crowdstrike.com/v1");
    assert_eq!(Cloud::Us2.endpoint(), "https://mcp.us-2.crowdstrike.com/v1");
    assert_eq!(Cloud::Eu1.endpoint(), "https://mcp.eu-1.crowdstrike.com/v1");
    assert!(serde_json::from_str::<Cloud>("\"us3\"").is_err());
}
#[test]
fn vscode_cimd_requires_the_documented_stable_minimum() {
    assert!(!vscode_supports_cimd("1.105.9\nsynthetic-commit\nx64\n"));
    assert!(vscode_supports_cimd("1.106.0\nsynthetic-commit\nx64\n"));
    assert!(vscode_supports_cimd("1.107.2"));
    assert!(!vscode_supports_cimd("1.106.0-insider"));
    assert!(!vscode_supports_cimd("unknown"));
}
#[test]
fn cimd_needs_exact_reviewed_identity_and_allowlist_confirmation() {
    let mut value = selection(ClientKind::Codex);
    value.registration = RegistrationMode::Cimd {
        metadata_url: "https://chatgpt.com/oauth/codex/client.json".into(),
        allowlist_confirmed: false,
    };
    assert_eq!(value.validate().unwrap_err().code, "SETUP_ALLOWLIST");
    for url in [
        "https://evil.example/client.json",
        "https://chatgpt.com/oauth/codex/../../evil/client.json",
        "https://chatgpt.com/oauth/codex/a/b/client.json",
    ] {
        value.registration = RegistrationMode::Cimd {
            metadata_url: url.into(),
            allowlist_confirmed: true,
        };
        assert_eq!(value.validate().unwrap_err().code, "SETUP_CIMD_IDENTITY");
    }
    selection(ClientKind::VsCode).validate().unwrap();
}
#[test]
fn identifier_and_callback_injection_are_rejected() {
    for name in ["--url", "a;whoami", "a b", "a\n", "$(whoami)", ""] {
        let mut value = selection(ClientKind::Codex);
        value.server_name = name.into();
        assert_eq!(value.validate().unwrap_err().code, "SETUP_SERVER_NAME");
    }
    for (client_id, callback_url) in [
        ("--secret", "http://127.0.0.1:1234/callback"),
        ("synthetic", "http://external.example/callback"),
        ("synthetic", "https://user:secret@example.com/callback"),
    ] {
        let mut value = selection(ClientKind::Codex);
        value.registration = RegistrationMode::Manual {
            client_id: client_id.into(),
            callback_url: callback_url.into(),
        };
        assert!(value.validate().is_err());
    }
}
#[test]
fn plans_bind_every_argv_change_and_state_revision() {
    let state = selected(ClientKind::Codex);
    let plan = build_plan(
        &state,
        PlanOperation::Configure,
        baseline(ExistingServer::Absent),
        None,
    )
    .unwrap();
    plan.validate().unwrap();
    assert_eq!(plan.commands.len(), 1);
    assert_eq!(
        plan.commands[0].args,
        [
            "mcp",
            "add",
            "cs-mcp",
            "--url",
            "https://mcp.us-2.crowdstrike.com/v1",
            "--oauth-client-registration",
            "cimd"
        ]
    );
    assert!(plan.commands[0].interactive);
    assert!(
        plan.changes
            .iter()
            .any(|c| c.contains("immediately open browser"))
    );
    let mut changed = plan.clone();
    changed.commands[0].args[2] = "other-name".into();
    assert_eq!(changed.validate().unwrap_err().code, "SETUP_PLAN_INTEGRITY");
    let mut changed = plan.clone();
    changed.state_revision += 1;
    assert_ne!(changed.digest().unwrap(), plan.confirmation_sha256);
}
#[test]
fn vscode_cimd_omits_oauth_client_id_and_manual_uses_nonsecret_id() {
    let mut state = selected(ClientKind::VsCode);
    let plan = build_plan(
        &state,
        PlanOperation::Configure,
        baseline(ExistingServer::Absent),
        None,
    )
    .unwrap();
    let native: serde_json::Value = serde_json::from_str(&plan.commands[0].args[1]).unwrap();
    assert!(native.get("oauth").is_none());
    assert_eq!(native["type"], "http");
    state.selection.as_mut().unwrap().registration = RegistrationMode::Manual {
        client_id: "synthetic-generated-id".into(),
        callback_url: "http://127.0.0.1:1234/callback".into(),
    };
    let plan = build_plan(
        &state,
        PlanOperation::Configure,
        baseline(ExistingServer::Absent),
        None,
    )
    .unwrap();
    let native: serde_json::Value = serde_json::from_str(&plan.commands[0].args[1]).unwrap();
    assert_eq!(native["oauth"]["clientId"], "synthetic-generated-id");
    assert!(!plan.commands[0].args.join(" ").contains("secret"));
}
#[test]
fn manual_codex_uses_only_locally_advertised_id_flag() {
    let mut state = selected(ClientKind::Codex);
    state.selection.as_mut().unwrap().registration = RegistrationMode::Manual {
        client_id: "synthetic-id".into(),
        callback_url: "http://127.0.0.1:1234/callback".into(),
    };
    let plan = build_plan(
        &state,
        PlanOperation::Configure,
        baseline(ExistingServer::Absent),
        None,
    )
    .unwrap();
    assert_eq!(
        &plan.commands[0].args[5..],
        ["--oauth-client-id", "synthetic-id"]
    );
    state.clients[0].supports_manual = false;
    assert_eq!(
        build_plan(
            &state,
            PlanOperation::Configure,
            baseline(ExistingServer::Absent),
            None
        )
        .unwrap_err()
        .code,
        "SETUP_CAPABILITY"
    );
}
#[test]
fn collisions_never_overwrite_and_same_config_skips_commands() {
    let state = selected(ClientKind::VsCode);
    assert_eq!(
        build_plan(
            &state,
            PlanOperation::Configure,
            baseline(ExistingServer::Collision),
            None
        )
        .unwrap_err()
        .code,
        "SETUP_COLLISION"
    );
    let plan = build_plan(
        &state,
        PlanOperation::Configure,
        baseline(ExistingServer::Same),
        None,
    )
    .unwrap();
    assert!(plan.commands.is_empty());
}
#[test]
fn unavailable_capability_never_falls_back_to_dcr() {
    let mut state = selected(ClientKind::Codex);
    state.clients[0].supports_cimd = false;
    assert_eq!(
        build_plan(
            &state,
            PlanOperation::Configure,
            baseline(ExistingServer::Absent),
            None
        )
        .unwrap_err()
        .code,
        "SETUP_CAPABILITY"
    );
    assert!(!serde_json::to_string(&state).unwrap().contains("dcr"));
}
#[test]
fn missing_client_and_service_mode_stay_pending() {
    let mut state = SetupState::default();
    state.select(selection(ClientKind::VsCode)).unwrap();
    assert_eq!(state.phase, SetupPhase::InstallationRequired);
    assert!(
        result(state.clone(), None).next_actions[0]
            .contains("https://code.visualstudio.com/download")
    );
    state
        .select(Selection {
            registration: RegistrationMode::ApiClient {
                credentials_file: std::env::temp_dir()
                    .join("never-read-credentials.json")
                    .to_string_lossy()
                    .into_owned(),
            },
            ..selection(ClientKind::VsCode)
        })
        .unwrap();
    assert_eq!(state.phase, SetupPhase::AdvancedPending);
    assert_eq!(
        build_plan(
            &state,
            PlanOperation::Configure,
            baseline(ExistingServer::Absent),
            None
        )
        .unwrap_err()
        .code,
        "SETUP_ADVANCED_PENDING"
    );
}
fn report() -> HostToolDiscovery {
    HostToolDiscovery {
        schema_version: 1,
        client: ClientKind::VsCode,
        server_name: "cs-mcp".into(),
        endpoint: Cloud::Us2.endpoint().into(),
        authenticated: true,
        tools: vec![DiscoveryTool {
            name: SEARCH_TOOL.into(),
            available: true,
        }],
    }
}
#[test]
fn discovery_requires_exact_available_tool_and_bound_client() {
    let mut state = selected(ClientKind::VsCode);
    assert_eq!(
        verify_discovery(&mut state, &report(), sha256_hex(b"report"))
            .unwrap_err()
            .code,
        "SETUP_NOT_CONFIGURED"
    );
    state.configured = true;
    for wrong in [
        HostToolDiscovery {
            endpoint: Cloud::Us1.endpoint().into(),
            ..report()
        },
        HostToolDiscovery {
            client: ClientKind::Codex,
            ..report()
        },
        HostToolDiscovery {
            authenticated: false,
            ..report()
        },
        HostToolDiscovery {
            server_name: "different-server".into(),
            ..report()
        },
    ] {
        assert_eq!(
            verify_discovery(&mut state, &wrong, sha256_hex(b"report"))
                .unwrap_err()
                .code,
            "SETUP_DISCOVERY_BINDING"
        );
    }
    for tool in [
        DiscoveryTool {
            name: "prefix_falcon_search_ngsiem".into(),
            available: true,
        },
        DiscoveryTool {
            name: SEARCH_TOOL.into(),
            available: false,
        },
    ] {
        let wrong = HostToolDiscovery {
            tools: vec![tool],
            ..report()
        };
        verify_discovery(&mut state, &wrong, sha256_hex(b"report")).unwrap();
        assert_eq!(state.phase, SetupPhase::Authenticated);
    }
    verify_discovery(&mut state, &report(), sha256_hex(b"report")).unwrap();
    state.validate().unwrap();
    assert_eq!(state.phase, SetupPhase::ToolDiscoveryVerified);
    assert_eq!(
        state.authentication_evidence,
        AuthenticationEvidence::SavedHostToolDiscovery
    );
    let handoff = result(state, None).handoff.unwrap();
    assert!(handoff.contains("cqlmatic env inventory"));
    assert!(handoff.contains("saved MCP results"));
}
#[test]
fn pending_native_action_blocks_reselection_and_survives_cancel() {
    let mut state = selected(ClientKind::VsCode);
    state.phase = SetupPhase::CommandPending;
    assert_eq!(
        state.select(selection(ClientKind::Codex)).unwrap_err().code,
        "SETUP_PENDING"
    );
    state.cancel();
    assert_eq!(state.phase, SetupPhase::CommandPending);
    assert_eq!(
        build_plan(
            &state,
            PlanOperation::Configure,
            baseline(ExistingServer::Absent),
            None
        )
        .unwrap_err()
        .code,
        "SETUP_PENDING"
    );
}
