use cqlmatic::{setup::*, sha256_hex};
use serde_json::json;

fn selection(client: ClientKind) -> Selection {
    Selection {
        client,
        host_id: None,
        cloud: Cloud::Us2,
        server_name: "cs-mcp".into(),
        registration: RegistrationMode::Cimd {
            metadata_url: match client {
                ClientKind::Codex => "https://chatgpt.com/oauth/codex/synthetic/client.json",
                ClientKind::VsCode => "https://vscode.dev/oauth/client-metadata.json",
                _ => panic!("native selection only"),
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
fn host_selection(client: ClientKind, host_id: Option<&str>) -> Selection {
    Selection {
        client,
        host_id: host_id.map(str::to_owned),
        cloud: Cloud::Eu1,
        server_name: "cs-mcp".into(),
        registration: RegistrationMode::HostManaged {},
    }
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
        include_str!("../examples/setup/select-claude.request.json"),
        include_str!("../examples/setup/select-copilot.request.json"),
        include_str!("../examples/setup/select-host.request.json"),
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
    let host_report: HostToolDiscovery =
        serde_json::from_str(include_str!("../examples/setup/tool-discovery-host.json")).unwrap();
    assert_eq!(host_report.client, ClientKind::External);
    assert_eq!(host_report.host_id.as_deref(), Some("example-agent"));
    for text in [
        include_str!("../examples/setup/tool-discovery-claude.json"),
        include_str!("../examples/setup/tool-discovery-copilot.json"),
    ] {
        let report: HostToolDiscovery = serde_json::from_str(text).unwrap();
        assert!(report.effective_oauth_client_id.is_some());
        assert!(report.effective_config_source.is_some());
    }
}
#[test]
fn claude_cimd_version_requires_supported_stable_release() {
    assert!(!claude_supports_cimd("2.1.80 (Claude Code)"));
    assert!(claude_supports_cimd("2.1.81 (Claude Code)"));
    assert!(claude_supports_cimd("2.1.314 (Claude Code)"));
    assert!(!claude_supports_cimd("2.1.81-beta"));
    assert!(!claude_supports_cimd("unknown"));
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
fn old_v1_native_state_and_preview_remain_readable() {
    let mut state = selected(ClientKind::Codex);
    state.plan = Some(
        build_plan(
            &state,
            PlanOperation::Configure,
            baseline(ExistingServer::Absent),
            None,
        )
        .unwrap(),
    );
    state.phase = SetupPhase::ReviewRequired;
    let old_shape = serde_json::to_value(&state).unwrap();
    assert!(old_shape["selection"].get("host_id").is_none());
    assert!(old_shape["plan"].get("config_write_sha256").is_none());
    assert!(old_shape["plan"]["client"].is_object());
    let loaded: SetupState = serde_json::from_value(old_shape).unwrap();
    loaded.validate().unwrap();
    assert_eq!(loaded.plan.unwrap().commands[0].args[0], "mcp");
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
        host_id: None,
        server_name: "cs-mcp".into(),
        endpoint: Cloud::Us2.endpoint().into(),
        authenticated: true,
        effective_oauth_client_id: None,
        effective_config_source: None,
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
    let wrong_identity = HostToolDiscovery {
        effective_oauth_client_id: Some("different-client".into()),
        ..report()
    };
    assert_eq!(
        verify_discovery(&mut state, &wrong_identity, sha256_hex(b"wrong identity"))
            .unwrap_err()
            .code,
        "SETUP_DISCOVERY_IDENTITY"
    );
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
fn host_managed_selection_is_tracked_without_native_detection_or_preview() {
    {
        let client = ClientKind::External;
        let host_id = (client == ClientKind::External).then_some("synthetic-host");
        let selection = host_selection(client, host_id);
        selection.validate().unwrap();
        let mut state = SetupState::default();
        state.select(selection.clone()).unwrap();
        assert_eq!(state.phase, SetupPhase::Selected);
        assert!(state.clients.is_empty());
        assert_eq!(
            build_plan(
                &state,
                PlanOperation::Configure,
                baseline(ExistingServer::Absent),
                None
            )
            .unwrap_err()
            .code,
            "SETUP_HOST_MANAGED"
        );
        let guidance = result(state.clone(), None).next_actions.join(" ");
        assert!(guidance.contains("HostToolDiscovery"));
        assert!(!guidance.contains("Preview the exact native-client"));
        state.cancel();
        assert!(
            !result(state, None)
                .next_actions
                .join(" ")
                .contains("Preview the exact native-client")
        );
    }
    let legacy = serde_json::to_value(selection(ClientKind::Codex)).unwrap();
    assert!(legacy.get("host_id").is_none());
    serde_json::from_value::<Selection>(legacy)
        .unwrap()
        .validate()
        .unwrap();
    for bad in [None, Some("bad host"), Some("--bad"), Some("a/b"), Some("")] {
        assert_eq!(
            host_selection(ClientKind::External, bad)
                .validate()
                .unwrap_err()
                .code,
            "SETUP_HOST_ID"
        );
    }
    assert_eq!(
        host_selection(ClientKind::ClaudeCode, Some("extra"))
            .validate()
            .unwrap_err()
            .code,
        "SETUP_HOST_ID"
    );
    let mut wrong_mode = host_selection(ClientKind::External, Some("synthetic-host"));
    wrong_mode.registration = selection(ClientKind::Codex).registration;
    assert_eq!(
        wrong_mode.validate().unwrap_err().code,
        "SETUP_REGISTRATION_MODE"
    );
    let mut wrong_mode = selection(ClientKind::Codex);
    wrong_mode.registration = RegistrationMode::HostManaged {};
    assert_eq!(
        wrong_mode.validate().unwrap_err().code,
        "SETUP_REGISTRATION_MODE"
    );
}
#[test]
fn host_managed_report_binds_surface_and_preserves_incomplete_discovery() {
    {
        let client = ClientKind::External;
        let host_id = (client == ClientKind::External).then_some("synthetic-host");
        let mut state = SetupState::default();
        state.select(host_selection(client, host_id)).unwrap();
        let mut report = HostToolDiscovery {
            schema_version: SCHEMA_VERSION,
            client,
            host_id: host_id.map(str::to_owned),
            server_name: "cs-mcp".into(),
            endpoint: Cloud::Eu1.endpoint().into(),
            authenticated: true,
            effective_oauth_client_id: Some("synthetic-external-client".into()),
            effective_config_source: Some("selected host session".into()),
            tools: vec![DiscoveryTool {
                name: SEARCH_TOOL.into(),
                available: false,
            }],
        };
        let original = serde_json::to_value(&state).unwrap();
        for wrong in [
            HostToolDiscovery {
                client: ClientKind::Codex,
                ..report.clone()
            },
            HostToolDiscovery {
                host_id: Some("other".into()),
                ..report.clone()
            },
            HostToolDiscovery {
                server_name: "other".into(),
                ..report.clone()
            },
            HostToolDiscovery {
                endpoint: Cloud::Us1.endpoint().into(),
                ..report.clone()
            },
            HostToolDiscovery {
                authenticated: false,
                ..report.clone()
            },
            HostToolDiscovery {
                tools: vec![
                    DiscoveryTool {
                        name: SEARCH_TOOL.into(),
                        available: true,
                    },
                    DiscoveryTool {
                        name: SEARCH_TOOL.into(),
                        available: true,
                    },
                ],
                ..report.clone()
            },
        ] {
            assert!(verify_discovery(&mut state, &wrong, sha256_hex(b"synthetic report")).is_err());
            assert_eq!(serde_json::to_value(&state).unwrap(), original);
        }
        verify_discovery(&mut state, &report, sha256_hex(b"missing tool")).unwrap();
        assert_eq!(state.phase, SetupPhase::Authenticated);
        assert!(state.configured);
        assert_eq!(state.configured_sha256, None);
        assert_eq!(
            state.authentication_evidence,
            AuthenticationEvidence::SavedHostToolDiscovery
        );
        report.tools[0].available = true;
        verify_discovery(&mut state, &report, sha256_hex(b"available tool")).unwrap();
        state.validate().unwrap();
        assert_eq!(state.phase, SetupPhase::ToolDiscoveryVerified);
        let handoff = result(state, None).handoff.unwrap();
        assert!(handoff.contains("search-all"));
        assert!(handoff.contains("separate explicit authorization"));
        if client == ClientKind::External {
            assert!(handoff.contains("host_id"));
        }
    }
}
#[test]
fn named_hosts_use_native_registration_and_bound_session_identity() {
    for client in [
        ClientKind::ClaudeCode,
        ClientKind::CopilotCli,
        ClientKind::CopilotApp,
        ClientKind::CopilotVscode,
    ] {
        let (registration, expected_id) = if client == ClientKind::ClaudeCode {
            let id = "https://claude.ai/oauth/claude-code-client-metadata";
            (
                RegistrationMode::Cimd {
                    metadata_url: id.into(),
                    allowlist_confirmed: true,
                },
                id,
            )
        } else {
            (
                RegistrationMode::Manual {
                    client_id: "synthetic-copilot-id".into(),
                    callback_url: "http://localhost:1234/".into(),
                },
                "synthetic-copilot-id",
            )
        };
        let selection = Selection {
            client,
            host_id: None,
            cloud: Cloud::Eu1,
            server_name: "cs-mcp".into(),
            registration,
        };
        selection.validate().unwrap();
        let mut state = SetupState::default();
        state.select(selection.clone()).unwrap();
        assert_eq!(
            state.phase,
            if client == ClientKind::ClaudeCode {
                SetupPhase::InstallationRequired
            } else {
                SetupPhase::Selected
            }
        );
        if client == ClientKind::ClaudeCode {
            state.clients.push(observation(client));
            state.select(selection).unwrap();
        }
        let plan = build_plan(
            &state,
            PlanOperation::Configure,
            baseline(ExistingServer::Absent),
            None,
        )
        .unwrap();
        if client == ClientKind::ClaudeCode {
            assert_eq!(
                &plan.commands[0].args[..6],
                ["mcp", "add", "--scope", "user", "--transport", "http"]
            );
        } else {
            assert!(plan.client.is_none());
            assert!(plan.commands.is_empty());
        }
        state.configured = true;
        let mut report = HostToolDiscovery {
            schema_version: 1,
            client,
            host_id: None,
            server_name: "cs-mcp".into(),
            endpoint: Cloud::Eu1.endpoint().into(),
            authenticated: true,
            effective_oauth_client_id: Some("wrong-id".into()),
            effective_config_source: Some("user".into()),
            tools: vec![DiscoveryTool {
                name: SEARCH_TOOL.into(),
                available: false,
            }],
        };
        assert_eq!(
            verify_discovery(&mut state, &report, sha256_hex(b"wrong identity"))
                .unwrap_err()
                .code,
            "SETUP_DISCOVERY_IDENTITY"
        );
        report.effective_oauth_client_id = Some(expected_id.into());
        report.effective_config_source = None;
        assert_eq!(
            verify_discovery(&mut state, &report, sha256_hex(b"missing source"))
                .unwrap_err()
                .code,
            "SETUP_DISCOVERY_IDENTITY"
        );
        report.effective_config_source = Some("selected session user config".into());
        verify_discovery(&mut state, &report, sha256_hex(b"missing tool")).unwrap();
        assert_eq!(state.phase, SetupPhase::Authenticated);
        report.tools[0].available = true;
        verify_discovery(&mut state, &report, sha256_hex(b"available tool")).unwrap();
        assert_eq!(state.phase, SetupPhase::ToolDiscoveryVerified);
    }
}
#[test]
fn claude_native_cimd_does_not_silently_configure_manual_registration() {
    let value = Selection {
        client: ClientKind::ClaudeCode,
        host_id: None,
        cloud: Cloud::Us1,
        server_name: "cs-mcp".into(),
        registration: RegistrationMode::Manual {
            client_id: "synthetic-id".into(),
            callback_url: "http://localhost:1234/callback".into(),
        },
    };
    assert_eq!(
        value.validate().unwrap_err().code,
        "SETUP_REGISTRATION_MODE"
    );
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
