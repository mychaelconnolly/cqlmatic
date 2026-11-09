//! Pure, strict contracts and planning for the native-client MCP setup wizard.
//!
//! Native clients own OAuth, tokens, MCP transport, and consent. These types never
//! perform I/O or promote a successful configuration command into authentication.
use crate::{contracts::Diagnostic, sha256_hex};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_STATE_BYTES: u64 = 256 * 1024;
pub const MAX_DISCOVERY_BYTES: u64 = 1024 * 1024;
pub const MAX_COMMAND_BYTES: usize = 256 * 1024;
pub const SEARCH_TOOL: &str = "falcon_search_ngsiem";
pub const SOURCE_REVIEWED: &str = "2026-11-06";
const HOST_SOURCES_REVIEWED: &str = "2026-11-08";
const VSCODE_CIMD_MINIMUM: (u32, u32, u32) = (1, 106, 0);

/// VS Code 1.106 introduced CIMD; prerelease/unknown version strings are inconclusive.
pub fn vscode_supports_cimd(version: &str) -> bool {
    let Some(line) = version.lines().next() else {
        return false;
    };
    let parts: Vec<_> = line.trim().split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    let parsed: Option<Vec<u32>> = parts.iter().map(|part| part.parse().ok()).collect();
    parsed.is_some_and(|parts| (parts[0], parts[1], parts[2]) >= VSCODE_CIMD_MINIMUM)
}

pub fn claude_supports_cimd(version: &str) -> bool {
    let version = version.split_whitespace().next().unwrap_or_default();
    let parts: Option<Vec<u32>> = version.split('.').map(|part| part.parse().ok()).collect();
    parts.is_some_and(|parts| parts.len() == 3 && (parts[0], parts[1], parts[2]) >= (2, 1, 81))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ClientKind {
    Codex,
    VsCode,
    ClaudeCode,
    CopilotCli,
    CopilotApp,
    CopilotVscode,
    External,
}
impl ClientKind {
    pub fn is_native(self) -> bool {
        self != Self::External
    }
    pub fn needs_executable(self) -> bool {
        matches!(self, Self::Codex | Self::VsCode | Self::ClaudeCode)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex CLI",
            Self::VsCode => "VS Code Local harness",
            Self::ClaudeCode => "Claude Code",
            Self::CopilotCli => "GitHub Copilot CLI",
            Self::CopilotApp => "GitHub Copilot app",
            Self::CopilotVscode => "VS Code Copilot harness",
            Self::External => "Other agent",
        }
    }
    pub fn install_url(self) -> Option<&'static str> {
        match self {
            Self::Codex => Some("https://developers.openai.com/codex/cli/"),
            Self::VsCode => Some("https://code.visualstudio.com/download"),
            Self::ClaudeCode => Some("https://code.claude.com/docs/en/setup"),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Cloud {
    Us1,
    Us2,
    Eu1,
}
impl Cloud {
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Us1 => "https://mcp.crowdstrike.com/v1",
            Self::Us2 => "https://mcp.us-2.crowdstrike.com/v1",
            Self::Eu1 => "https://mcp.eu-1.crowdstrike.com/v1",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RegistrationMode {
    Cimd {
        metadata_url: String,
        allowlist_confirmed: bool,
    },
    Manual {
        client_id: String,
        callback_url: String,
    },
    ApiClient {
        credentials_file: String,
    },
    HostManaged {},
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub client: ClientKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    pub cloud: Cloud,
    pub server_name: String,
    pub registration: RegistrationMode,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ClientObservation {
    pub client: ClientKind,
    /// Absolute, canonical executable discovered locally; never accepted from a request.
    pub executable: String,
    pub executable_sha256: String,
    pub version: String,
    pub supports_http: bool,
    pub supports_cimd: bool,
    pub supports_manual: bool,
    pub supports_login: bool,
    pub capability_sha256: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum SetupPhase {
    New,
    Selected,
    InstallationRequired,
    ReviewRequired,
    CommandPending,
    AuthenticationRequired,
    Authenticated,
    ToolDiscoveryVerified,
    AdvancedPending,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum AuthenticationEvidence {
    None,
    UserConfirmed,
    SavedHostToolDiscovery,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum PlanOperation {
    Configure,
    Authenticate,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NativeCommand {
    pub program: String,
    pub args: Vec<String>,
    pub interactive: bool,
    pub timeout_seconds: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ConfigBaseline {
    pub path: String,
    /// None means absent. A zero-byte existing file still has a digest.
    pub sha256: Option<String>,
    pub server: ExistingServer,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ExistingServer {
    Absent,
    Same,
    Collision,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SetupPlan {
    pub schema_version: u32,
    pub state_revision: u64,
    pub operation: PlanOperation,
    pub selection: Selection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientObservation>,
    pub baseline: ConfigBaseline,
    pub commands: Vec<NativeCommand>,
    pub changes: Vec<String>,
    pub backup_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_write_sha256: Option<String>,
    pub confirmation_sha256: String,
}
impl SetupPlan {
    pub fn digest(&self) -> Result<String, Diagnostic> {
        let mut payload = self.clone();
        payload.confirmation_sha256.clear();
        let bytes = serde_json::to_vec(&payload)
            .map_err(|_| error("SETUP_SERIALIZATION", "Cannot fingerprint setup plan."))?;
        Ok(sha256_hex(&bytes))
    }
    pub fn validate(&self) -> Result<(), Diagnostic> {
        self.selection.validate()?;
        if self.schema_version != SCHEMA_VERSION
            || self
                .config_write_sha256
                .as_deref()
                .is_some_and(|hash| !is_digest(hash))
            || self.confirmation_sha256 != self.digest()?
        {
            return Err(error(
                "SETUP_PLAN_INTEGRITY",
                "Setup plan is invalid or changed. Preview again.",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SetupState {
    pub schema_version: u32,
    pub revision: u64,
    pub phase: SetupPhase,
    pub selection: Option<Selection>,
    pub clients: Vec<ClientObservation>,
    pub plan: Option<SetupPlan>,
    pub configured: bool,
    pub authentication_evidence: AuthenticationEvidence,
    pub configured_sha256: Option<String>,
    pub discovery_sha256: Option<String>,
    pub last_notice: Option<String>,
}
impl Default for SetupState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            phase: SetupPhase::New,
            selection: None,
            clients: Vec::new(),
            plan: None,
            configured: false,
            authentication_evidence: AuthenticationEvidence::None,
            configured_sha256: None,
            discovery_sha256: None,
            last_notice: None,
        }
    }
}
impl SetupState {
    pub fn validate(&self) -> Result<(), Diagnostic> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(error(
                "SETUP_VERSION",
                "Only setup schema_version 1 is supported.",
            ));
        }
        if let Some(selection) = &self.selection {
            selection.validate()?;
        }
        for client in &self.clients {
            if !Path::new(&client.executable).is_absolute()
                || !is_digest(&client.executable_sha256)
                || !is_digest(&client.capability_sha256)
                || client.version.len() > 160
            {
                return Err(error(
                    "SETUP_STATE",
                    "Stored client observation is invalid. Re-detect clients.",
                ));
            }
        }
        if let Some(plan) = &self.plan {
            plan.validate()?;
            if self.selection.as_ref() != Some(&plan.selection)
                || plan.state_revision != self.revision
            {
                return Err(error(
                    "SETUP_STATE",
                    "Stored plan does not match the current selection and revision.",
                ));
            }
        }
        if self.configured && self.selection.is_none() {
            return Err(error(
                "SETUP_STATE",
                "Configured setup needs a selected client.",
            ));
        }
        if self.phase == SetupPhase::ToolDiscoveryVerified
            && (!self.configured
                || self
                    .discovery_sha256
                    .as_deref()
                    .is_none_or(|s| !is_digest(s))
                || self.authentication_evidence != AuthenticationEvidence::SavedHostToolDiscovery)
        {
            return Err(error(
                "SETUP_STATE",
                "Verified setup requires bound host tool-discovery evidence.",
            ));
        }
        Ok(())
    }
    pub fn select(&mut self, selection: Selection) -> Result<(), Diagnostic> {
        selection.validate()?;
        if self.phase == SetupPhase::CommandPending {
            return Err(error(
                "SETUP_PENDING",
                "A prior native command may have changed configuration. Review it before changing selection.",
            ));
        }
        self.revision += 1;
        self.selection = Some(selection.clone());
        self.plan = None;
        self.configured = false;
        self.configured_sha256 = None;
        self.discovery_sha256 = None;
        self.authentication_evidence = AuthenticationEvidence::None;
        self.last_notice = None;
        self.phase = if matches!(selection.registration, RegistrationMode::ApiClient { .. }) {
            SetupPhase::AdvancedPending
        } else if !selection.client.needs_executable()
            || self.clients.iter().any(|c| c.client == selection.client)
        {
            SetupPhase::Selected
        } else {
            SetupPhase::InstallationRequired
        };
        Ok(())
    }
    pub fn invalidate_plan(&mut self) {
        self.revision += 1;
        self.plan = None;
    }
    pub fn cancel(&mut self) {
        if self.phase != SetupPhase::CommandPending {
            self.invalidate_plan();
            self.phase = SetupPhase::Cancelled;
        }
        self.last_notice = Some("Cancelled. Saved progress remains; resume with cqlmatic setup. A pending native command requires inspection before another attempt.".into());
    }
}
impl Selection {
    pub fn validate(&self) -> Result<(), Diagnostic> {
        if !identifier(&self.server_name) {
            return Err(error(
                "SETUP_SERVER_NAME",
                "Server name must be 1-64 ASCII letters, digits, underscores or hyphens and start with a letter or digit.",
            ));
        }
        if self.client == ClientKind::External {
            if !self.host_id.as_deref().is_some_and(identifier) {
                return Err(error(
                    "SETUP_HOST_ID",
                    "Other agents need a 1-64 character ASCII host_id using letters, digits, underscores or hyphens.",
                ));
            }
        } else if self.host_id.is_some() {
            return Err(error(
                "SETUP_HOST_ID",
                "host_id is only valid when client is external.",
            ));
        }
        if self.client.is_native() == matches!(self.registration, RegistrationMode::HostManaged {})
        {
            return Err(error(
                "SETUP_REGISTRATION_MODE",
                "Named agents use native registration; external agents use host-managed registration.",
            ));
        }
        match &self.registration {
            RegistrationMode::Cimd {
                metadata_url,
                allowlist_confirmed,
            } => {
                if !*allowlist_confirmed {
                    return Err(error(
                        "SETUP_ALLOWLIST",
                        "Confirm the exact client metadata identity is currently allowed by CrowdStrike before selecting CIMD.",
                    ));
                }
                let codex_id = metadata_url.strip_prefix("https://chatgpt.com/oauth/codex/");
                let allowed = match self.client {
                    ClientKind::VsCode => {
                        metadata_url == "https://vscode.dev/oauth/client-metadata.json"
                    }
                    ClientKind::ClaudeCode => {
                        metadata_url == "https://claude.ai/oauth/claude-code-client-metadata"
                    }
                    ClientKind::Codex => codex_id.is_some_and(|tail| {
                        tail == "client.json"
                            || tail.strip_suffix("/client.json").is_some_and(identifier)
                    }),
                    _ => false,
                };
                if !allowed {
                    return Err(error(
                        "SETUP_CIMD_IDENTITY",
                        "Use the exact published native-client metadata URL. Codex identity depends on client version and server capabilities; inspect it in the client and confirm allowlisting.",
                    ));
                }
            }
            RegistrationMode::Manual {
                client_id,
                callback_url,
            } => {
                if self.client == ClientKind::ClaudeCode {
                    return Err(error(
                        "SETUP_REGISTRATION_MODE",
                        "Claude Code's native setup uses its allowlisted CIMD identity; select CIMD for this adapter.",
                    ));
                }
                if client_id.is_empty()
                    || client_id.len() > 256
                    || !client_id.as_bytes()[0].is_ascii_alphanumeric()
                    || !client_id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
                {
                    return Err(error(
                        "SETUP_CLIENT_ID",
                        "Manual client ID must be a non-secret ASCII identifier, not an option or URL.",
                    ));
                }
                let url = reqwest::Url::parse(callback_url).map_err(|_| {
                    error(
                        "SETUP_CALLBACK",
                        "Provide the exact callback URI reported by your native client.",
                    )
                })?;
                let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
                if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                    || callback_url.len() > 512
                {
                    return Err(error(
                        "SETUP_CALLBACK",
                        "Use the native client's exact HTTPS or HTTP loopback callback URI, without embedded credentials or fragments.",
                    ));
                }
            }
            RegistrationMode::ApiClient { credentials_file } => {
                if !Path::new(credentials_file).is_absolute() || credentials_file.len() > 4096 {
                    return Err(error(
                        "SETUP_CREDENTIAL_REFERENCE",
                        "Provide an absolute protected credential-file path. Setup records the reference and never reads the credentials.",
                    ));
                }
            }
            RegistrationMode::HostManaged {} => {}
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SetupRequest {
    pub schema_version: u32,
    #[serde(default)]
    pub id: Option<String>,
    pub action: SetupAction,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SetupAction {
    Status {},
    Detect {},
    Select { selection: Selection },
    Preview { operation: PlanOperation },
    Apply { confirmation_sha256: String },
    ConfirmAuthentication {},
    ImportDiscovery { file: String },
    AcknowledgePending { confirmation_sha256: String },
    Cancel {},
}
impl SetupRequest {
    pub fn validate(&self) -> Result<(), Diagnostic> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(error(
                "SETUP_VERSION",
                "Only setup schema_version 1 is supported.",
            ));
        }
        if self
            .id
            .as_ref()
            .is_some_and(|id| id.len() > 256 || id.chars().any(char::is_control))
        {
            return Err(error(
                "SETUP_REQUEST",
                "Request id is too long or contains control characters.",
            ));
        }
        match &self.action {
            SetupAction::Select { selection } => selection.validate(),
            SetupAction::Apply {
                confirmation_sha256,
            }
            | SetupAction::AcknowledgePending {
                confirmation_sha256,
            } if !is_digest(confirmation_sha256) => Err(error(
                "SETUP_CONFIRMATION",
                "The action needs the exact 64-character lowercase SHA256 from the reviewed preview.",
            )),
            SetupAction::ImportDiscovery { file } if file.is_empty() || file.len() > 4096 => {
                Err(error(
                    "SETUP_DISCOVERY_FILE",
                    "Supply a saved host tool-discovery file.",
                ))
            }
            _ => Ok(()),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DiscoveryTool {
    pub name: String,
    pub available: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct HostToolDiscovery {
    pub schema_version: u32,
    pub client: ClientKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    pub server_name: String,
    pub endpoint: String,
    pub authenticated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_oauth_client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_config_source: Option<String>,
    pub tools: Vec<DiscoveryTool>,
}
/// Match the saved host report exactly; strings containing a tool name are not evidence.
pub fn verify_discovery(
    state: &mut SetupState,
    report: &HostToolDiscovery,
    raw_sha256: String,
) -> Result<(), Diagnostic> {
    let selected = state
        .selection
        .as_ref()
        .ok_or_else(|| error("SETUP_SELECTION", "Choose a client and cloud first."))?;
    let host_managed = !selected.client.is_native();
    if (selected.client.is_native() && !state.configured)
        || state.phase == SetupPhase::CommandPending
    {
        return Err(error(
            "SETUP_NOT_CONFIGURED",
            "Complete configuration and inspect any pending native command first.",
        ));
    }
    if report.schema_version != SCHEMA_VERSION
        || report.client != selected.client
        || report.host_id.as_deref() != selected.host_id.as_deref()
        || report.server_name != selected.server_name
        || report.endpoint != selected.cloud.endpoint()
        || !report.authenticated
        || !is_digest(&raw_sha256)
        || report.tools.len() > 4096
    {
        return Err(error(
            "SETUP_DISCOVERY_BINDING",
            "Saved host report must state authentication for the exact selected client, host ID, server name, and canonical cloud endpoint.",
        ));
    }
    let expected_id = match &selected.registration {
        RegistrationMode::Cimd { metadata_url, .. } => Some(metadata_url.as_str()),
        RegistrationMode::Manual { client_id, .. } => Some(client_id.as_str()),
        _ => None,
    };
    if expected_id.is_some()
        && report
            .effective_oauth_client_id
            .as_deref()
            .is_some_and(|id| Some(id) != expected_id)
    {
        return Err(error(
            "SETUP_DISCOVERY_IDENTITY",
            "The host report contradicts the selected OAuth client identity.",
        ));
    }
    if matches!(
        selected.client,
        ClientKind::ClaudeCode
            | ClientKind::CopilotCli
            | ClientKind::CopilotApp
            | ClientKind::CopilotVscode
    ) && (report.effective_oauth_client_id.as_deref() != expected_id
        || !report
            .effective_config_source
            .as_deref()
            .is_some_and(|source| {
                !source.is_empty() && source.len() <= 256 && !source.chars().any(char::is_control)
            }))
    {
        return Err(error(
            "SETUP_DISCOVERY_IDENTITY",
            "The selected host session must report its effective OAuth client ID and configuration source, matching the selected registration. Caller-supplied evidence is not independent attestation.",
        ));
    }
    let mut names = std::collections::BTreeSet::new();
    for tool in &report.tools {
        if tool.name.len() > 256 || !names.insert(&tool.name) {
            return Err(error(
                "SETUP_DISCOVERY_TOOLS",
                "Tool report contains duplicate or oversized names.",
            ));
        }
    }
    let has_search_tool = report
        .tools
        .iter()
        .any(|tool| tool.name == SEARCH_TOOL && tool.available);
    state.invalidate_plan();
    if host_managed {
        state.configured = true;
        state.configured_sha256 = None;
    }
    state.authentication_evidence = AuthenticationEvidence::SavedHostToolDiscovery;
    state.discovery_sha256 = Some(raw_sha256);
    state.phase = if has_search_tool {
        SetupPhase::ToolDiscoveryVerified
    } else {
        SetupPhase::Authenticated
    };
    state.last_notice = Some(if has_search_tool { "Saved host tool-discovery report matches this client and endpoint; no live query was run. Saved reports are caller-supplied evidence, not independent transport or configuration attestation." } else { "Saved host report states authentication, but falcon_search_ngsiem is not available. Tool discovery remains unverified. Check the host's selected tools, Falcon roles, and consent; no live query was run." }.into());
    Ok(())
}

pub fn build_plan(
    state: &SetupState,
    operation: PlanOperation,
    baseline: ConfigBaseline,
    backup_path: Option<String>,
) -> Result<SetupPlan, Diagnostic> {
    state.validate()?;
    let selection = state
        .selection
        .clone()
        .ok_or_else(|| error("SETUP_SELECTION", "Choose a client and cloud first."))?;
    if !selection.client.is_native() {
        return Err(error(
            "SETUP_HOST_MANAGED",
            "This agent uses its own MCP configuration. Configure and sign in there, then import a saved host tool-discovery report; no native plan is available.",
        ));
    }
    if matches!(selection.registration, RegistrationMode::ApiClient { .. }) {
        return Err(error(
            "SETUP_ADVANCED_PENDING",
            "Service mode needs a native client or approved plugin supporting OAuth client credentials. Complete its protected UI configuration; CQLmatic does not handle tokens or read this credential reference.",
        ));
    }
    if state.phase == SetupPhase::CommandPending {
        return Err(error(
            "SETUP_PENDING",
            "A native command was interrupted or failed and may have partially changed configuration. Inspect the client, acknowledge the pending plan hash, then preview the current baseline.",
        ));
    }
    let client = if selection.client.needs_executable() {
        state
            .clients
            .iter()
            .find(|c| c.client == selection.client)
            .cloned()
    } else {
        None
    };
    if selection.client.needs_executable() && client.is_none() {
        return Err(error(
            "SETUP_INSTALL",
            format!(
                "Install {} from {}, then detect again. No installer is run by CQLmatic.",
                selection.client.label(),
                selection
                    .client
                    .install_url()
                    .unwrap_or("its official site")
            ),
        ));
    }
    if client.as_ref().is_some_and(|client| !client.supports_http) {
        return Err(error(
            "SETUP_CAPABILITY",
            "Installed native client does not advertise the required HTTP MCP configuration capability. Update it and detect again.",
        ));
    }
    if matches!(selection.registration, RegistrationMode::Cimd { .. })
        && client.as_ref().is_none_or(|client| !client.supports_cimd)
    {
        return Err(error(
            "SETUP_CAPABILITY",
            "Installed client does not support the selected CIMD flow. VS Code requires stable 1.106.0 or later; Codex must advertise the CIMD flags. Update the client or explicitly choose manual registration; no DCR fallback is attempted.",
        ));
    }
    if selection.client.needs_executable()
        && matches!(selection.registration, RegistrationMode::Manual { .. })
        && client.as_ref().is_none_or(|client| !client.supports_manual)
    {
        return Err(error(
            "SETUP_CAPABILITY",
            "Installed client does not advertise a supported manual-registration adapter. Update it or use its protected native configuration UI.",
        ));
    }
    if matches!(baseline.server, ExistingServer::Collision) {
        return Err(error(
            "SETUP_COLLISION",
            "The selected server name already has a different configuration. Choose a new name; setup never overwrites collisions.",
        ));
    }
    let mut commands = Vec::new();
    let mut changes = vec![
        format!(
            "Selected server: {} -> {}",
            selection.server_name,
            selection.cloud.endpoint()
        ),
        format!(
            "Client config target: {} (unrelated entries preserved)",
            baseline.path
        ),
    ];
    if let Some(client) = &client {
        changes.insert(0, format!("Native client: {}", client.executable));
    }
    match operation {
        PlanOperation::Configure => {
            if baseline.server == ExistingServer::Same {
                changes.push(
                    "Desired server already exists; skip native configuration command.".into(),
                );
            } else {
                if selection.client.needs_executable() {
                    let client = client.as_ref().expect("checked above");
                    let args = configure_args(&selection, client)?;
                    let interactive = selection.client == ClientKind::Codex;
                    commands.push(NativeCommand {
                        program: client.executable.clone(),
                        args,
                        interactive,
                        timeout_seconds: if interactive { 300 } else { 30 },
                    });
                    changes.push(
                        "Add only the named HTTP MCP server through the native client command."
                            .into(),
                    );
                    if interactive {
                        changes.push("Codex add can immediately open browser login/consent and update native credential storage. Complete those steps in Codex; configuration success still does not attest authentication.".into());
                    }
                } else {
                    changes.push(format!("Create a private Copilot user configuration with one HTTP MCP server, the reviewed oauthClientId, and only {SEARCH_TOOL} enabled before connecting."));
                }
                if let Some(path) = &backup_path {
                    changes.push(format!("Protected original-config backup: {path}"));
                } else {
                    changes.push(
                        "Config file is currently absent; no original-config backup is needed."
                            .into(),
                    );
                }
            }
        }
        PlanOperation::Authenticate => {
            if !state.configured || baseline.server != ExistingServer::Same {
                return Err(error(
                    "SETUP_NOT_CONFIGURED",
                    "Configure this exact server before authenticating.",
                ));
            }
            match selection.client {
                ClientKind::Codex if client.as_ref().is_some_and(|client| client.supports_login) => {
                    let client = client.as_ref().expect("checked above");
                    let args = match &selection.registration {
                        RegistrationMode::Cimd { .. } if client.supports_cimd => vec!["mcp".into(), "login".into(), selection.server_name.clone(), "--oauth-client-registration".into(), "cimd".into()],
                        RegistrationMode::Manual { .. } => return Err(error("SETUP_CAPABILITY", "The Codex adapter cannot inspect persisted manual client IDs or select an explicit client ID through its supported login flow. Reauthenticate this registration in Codex; no login command was generated.")),
                        _ => return Err(error("SETUP_CAPABILITY", "Client does not advertise the required native OAuth flow.")),
                    };
                    commands.push(NativeCommand { program: client.executable.clone(), args, interactive: true, timeout_seconds: 300 });
                    changes.push("Native client opens browser login and consent and stores its own OAuth credentials. Secrets never enter CQLmatic.".into());
                }
                _ => changes.push("In the selected client's MCP UI: Start the server, Trust, Allow authentication, then complete Falcon login and consent. No automated authentication command is available.".into()),
            }
        }
    }
    let mut plan = SetupPlan {
        schema_version: SCHEMA_VERSION,
        state_revision: state.revision,
        operation,
        selection,
        client,
        baseline,
        commands,
        changes,
        backup_path,
        config_write_sha256: None,
        confirmation_sha256: String::new(),
    };
    plan.confirmation_sha256 = plan.digest()?;
    Ok(plan)
}
fn configure_args(
    selection: &Selection,
    client: &ClientObservation,
) -> Result<Vec<String>, Diagnostic> {
    match selection.client {
        ClientKind::Codex => {
            let mut args = vec![
                "mcp".into(),
                "add".into(),
                selection.server_name.clone(),
                "--url".into(),
                selection.cloud.endpoint().into(),
            ];
            match &selection.registration {
                RegistrationMode::Cimd { .. } if client.supports_cimd => {
                    args.extend(["--oauth-client-registration".into(), "cimd".into()])
                }
                RegistrationMode::Manual { client_id, .. } if client.supports_manual => {
                    args.extend(["--oauth-client-id".into(), client_id.clone()])
                }
                _ => {
                    return Err(error(
                        "SETUP_CAPABILITY",
                        "Installed client does not advertise CIMD. Use manual native-client setup or update the client.",
                    ));
                }
            }
            Ok(args)
        }
        ClientKind::VsCode => {
            let mut config = serde_json::json!({"name": selection.server_name, "type": "http", "url": selection.cloud.endpoint()});
            if let RegistrationMode::Manual { client_id, .. } = &selection.registration {
                config["oauth"] = serde_json::json!({"clientId":client_id});
            }
            Ok(vec![
                "--add-mcp".into(),
                serde_json::to_string(&config).map_err(|_| {
                    error("SETUP_SERIALIZATION", "Cannot serialize native MCP config.")
                })?,
            ])
        }
        ClientKind::ClaudeCode
            if matches!(selection.registration, RegistrationMode::Cimd { .. }) =>
        {
            Ok(vec![
                "mcp".into(),
                "add".into(),
                "--scope".into(),
                "user".into(),
                "--transport".into(),
                "http".into(),
                selection.server_name.clone(),
                selection.cloud.endpoint().into(),
            ])
        }
        _ => Err(error(
            "SETUP_HOST_MANAGED",
            "Host-managed agents have no native configuration command.",
        )),
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SourceMetadata {
    pub reviewed_on: String,
    pub title: String,
    pub url: String,
    pub uncertainty: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SetupResult {
    pub schema_version: u32,
    pub id: Option<String>,
    pub state: SetupState,
    pub next_actions: Vec<String>,
    pub handoff: Option<String>,
    pub sources: Vec<SourceMetadata>,
    pub diagnostics: Vec<Diagnostic>,
}
pub fn result(state: SetupState, id: Option<String>) -> SetupResult {
    let mut next_actions = Vec::new();
    let mut handoff = None;
    let selected = state.selection.as_ref();
    match state.phase {
        SetupPhase::New => next_actions.push("Choose Codex, VS Code Local, Claude Code, a GitHub Copilot surface, or another agent, then select the Falcon cloud (us1, us2, eu1). Claude/Codex/VS Code Local need local executable detection; Copilot's shared configuration adapter does not.".into()),
        SetupPhase::InstallationRequired => {
            if let Some(s) = selected && let Some(url) = s.client.install_url() { next_actions.push(format!("Install {} from {}; then run cqlmatic setup again. No installer is run automatically.", s.client.label(), url)); }
        }
        SetupPhase::Selected | SetupPhase::ReviewRequired | SetupPhase::Cancelled => {
            if let Some(s) = selected.filter(|s| !s.client.is_native()) {
                next_actions.push(format!("Configure '{}' as an HTTP MCP server at {} in {}. Confirm the effective server and supported OAuth identity before connecting; do not fall back to DCR.", s.server_name, s.cloud.endpoint(), s.client.label()));
                next_actions.push(host_setup_hint(s.client).into());
                next_actions.push("Save a caller-supplied v1 HostToolDiscovery report for this host/server/endpoint and import it with cqlmatic setup; setup does not inspect host configuration or run a query.".into());
            } else {
                next_actions.push("Preview the exact host configuration action and local changes; apply only with that preview's confirmation_sha256.".into());
                if let Some(s) = selected && matches!(s.client, ClientKind::ClaudeCode | ClientKind::CopilotCli | ClientKind::CopilotApp | ClientKind::CopilotVscode) {
                    next_actions.push(host_setup_hint(s.client).into());
                }
            }
        }
        SetupPhase::CommandPending => next_actions.push("Inspect the selected native client's configuration and authentication UI. The last native command may have partially changed it. Setup will not retry or claim rollback. Preview after local review acknowledges the current baseline.".into()),
        SetupPhase::AuthenticationRequired => {
            next_actions.push("Complete Falcon sign-in and consent in the selected native client. Its successful configuration command does not prove authentication.".into());
            next_actions.push("Ask the selected host session to report its effective OAuth client ID and configuration source, then save tool discovery for this exact client/server/endpoint; import it to confirm falcon_search_ngsiem is available.".into());
        }
        SetupPhase::Authenticated => next_actions.push("Authentication was reported, but falcon_search_ngsiem is not available. Check the selected host's effective server, Falcon roles, enabled tools and consent, then import a fresh bound report.".into()),
        SetupPhase::ToolDiscoveryVerified => next_actions.push("Paste the provided handoff into your selected host agent. It will guide environment inventory using cqlmatic env inventory and saved MCP results.".into()),
        SetupPhase::AdvancedPending => next_actions.push("Advanced API-client mode remains pending. Configure the native client's approved service plugin in its protected UI. Use the minimum read scopes; setup never reads your credential reference, obtains tokens, or manages MCP transport.".into()),
    }
    if let Some(s) = selected {
        let host_name = s.host_id.as_deref().unwrap_or(s.client.label());
        let host_id_field = if s.host_id.is_some() { "/host_id" } else { "" };
        handoff = Some(format!(
            "Continue CQLmatic setup in {} using MCP server '{}' at {}. {} First inspect the effective server and OAuth client ID in this host session, then discover tools and save a v1 HostToolDiscovery report with client{}/server_name/endpoint/authenticated/effective_oauth_client_id/effective_config_source/tools for `cqlmatic setup` import; exact available falcon_search_ngsiem is required for verified discovery. Reports are caller-supplied evidence, not independent transport or identity attestation. Run `cqlmatic env inventory --help`, then guide me through bounded inventory with explicit cloud/tenant/time and protected local output paths. You perform model reasoning and authorized MCP calls; relay saved MCP results into CQLmatic's offline flow. The built-in inventory scout emits repository=search-all and needs separate explicit authorization for that target and window. Without it, leave scouting pending and use lower-level selected-source discovery on the confirmed view; later query execution must retain that view. Use absolute times, read-only tools, and bounded searches. Registration and browser consent remain my steps. Do not run a live query merely to mark setup complete.",
            host_name,
            s.server_name,
            s.cloud.endpoint(),
            if state.phase == SetupPhase::ToolDiscoveryVerified {
                "Saved host discovery was imported; recheck host session freshness before use."
            } else {
                "Host configuration or authentication has not been independently attested."
            },
            host_id_field,
        ));
    }
    SetupResult { schema_version: SCHEMA_VERSION, id, state, next_actions, handoff, diagnostics: Vec::new(), sources: vec![
        SourceMetadata { reviewed_on: SOURCE_REVIEWED.into(), title: "CrowdStrike MCP documentation".into(), url: "https://falcon.crowdstrike.com/documentation/".into(), uncertainty: "Endpoint and CIMD guidance is a dated snapshot, not tenant allowlist or role detection. Confirm the exact identity and cloud in current Falcon documentation.".into() },
        SourceMetadata { reviewed_on: SOURCE_REVIEWED.into(), title: "Codex MCP native client".into(), url: "https://developers.openai.com/codex/mcp/".into(), uncertainty: "Codex CIMD metadata URLs depend on version and server capabilities; no universal metadata ID is assumed. Local --help gates commands.".into() },
        SourceMetadata { reviewed_on: SOURCE_REVIEWED.into(), title: "VS Code MCP servers".into(), url: "https://code.visualstudio.com/docs/copilot/customization/mcp-servers".into(), uncertainty: "--add-mcp targets the default user profile. Custom profiles, portable installs and remote extension hosts require manual native-client review.".into() },
        SourceMetadata { reviewed_on: SOURCE_REVIEWED.into(), title: "VS Code 1.106 CIMD support (October 2025)".into(), url: "https://code.visualstudio.com/updates/v1_106".into(), uncertainty: "CIMD requires stable VS Code 1.106.0 or later plus local --add-mcp capability. This does not establish CrowdStrike allowlisting or Falcon user roles.".into() },
        SourceMetadata { reviewed_on: HOST_SOURCES_REVIEWED.into(), title: "Claude Code MCP".into(), url: "https://code.claude.com/docs/en/mcp".into(), uncertainty: "Configuration scope, managed/session overrides, callback behavior and OAuth support vary by version; review in the selected host.".into() },
        SourceMetadata { reviewed_on: HOST_SOURCES_REVIEWED.into(), title: "GitHub Copilot CLI MCP".into(), url: "https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference".into(), uncertainty: "The user config can be overridden by session, plugin or workspace entries; confirm the effective identity in the selected session.".into() },
        SourceMetadata { reviewed_on: HOST_SOURCES_REVIEWED.into(), title: "GitHub Copilot app MCP".into(), url: "https://docs.github.com/en/copilot/how-tos/github-copilot-app/customize-github-copilot-app".into(), uncertainty: "The app may inherit CLI or repository servers; verify the server actually selected in the session.".into() },
        SourceMetadata { reviewed_on: HOST_SOURCES_REVIEWED.into(), title: "VS Code Copilot MCP".into(), url: "https://code.visualstudio.com/docs/agent-customization/mcp-servers".into(), uncertainty: "Copilot and Local harness configuration destinations differ; select and inspect the intended harness.".into() },
        SourceMetadata { reviewed_on: HOST_SOURCES_REVIEWED.into(), title: "GitHub CLI Copilot launcher".into(), url: "https://cli.github.com/manual/gh_copilot".into(), uncertainty: "gh copilot launches Copilot CLI and can download it when absent; CQLmatic never invokes this launcher.".into() },
    ] }
}
fn host_setup_hint(client: ClientKind) -> &'static str {
    match client {
        ClientKind::ClaudeCode => {
            "Claude Code: native user-scope configuration uses its CIMD identity when allowlisted. Check managed, local, project and session overrides, then sign in through /mcp. See docs/SETUP.md."
        }
        ClientKind::CopilotCli => {
            "Copilot CLI: copilot (including on Windows) and gh copilot launch the same CLI. Confirm this session's effective oauthClientId after the shared user configuration is written. See docs/SETUP.md."
        }
        ClientKind::CopilotApp => {
            "Copilot app: the shared user configuration is available in the app; confirm the entry and oauthClientId in Customize > MCP before connecting. See docs/SETUP.md."
        }
        ClientKind::CopilotVscode => {
            "VS Code Copilot harness: select Copilot and confirm the shared Copilot Global entry and oauthClientId; the VS Code Local mcp.json adapter is distinct. See docs/SETUP.md."
        }
        ClientKind::External => {
            "Other agent: verify a supported OAuth registration and effective server identity, plus local CLI, protected-file and saved-result access. Otherwise leave setup pending. See docs/SETUP.md."
        }
        _ => "",
    }
}
pub fn is_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
fn error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(code, message)
}
