# Setup and authentication

With a [supported, patched Node.js release](../README.md#try-v010) on macOS Apple silicon or Linux x86_64, use `npx cqlmatic@0.1.0 setup` from outside a CQLmatic source checkout. Use your source-built executable inside that checkout: npm can select the local package, which has no bundled executable. The npm package runs the native executable on demand without installing a global command. The guides use `cqlmatic` as shorthand for that pinned command or your standalone executable. Linux requires glibc 2.34 or newer and `libgcc_s.so.1`. Windows x64 uses the [release ZIP](#install-on-windows); other architectures use a [source build](DEVELOPMENT.md).

Run `npx` in a foreground terminal. For unattended process supervision, use the native executable directly: npm can add stderr notices, and cancellation directed only at its parent PID may not reach the command. See [input and output rules](../USAGE.md#input-and-output-rules).

Run `cqlmatic setup` in a terminal. The wizard detects supported local clients, asks for your Falcon cloud and connection choice, previews the native command and local destination, and asks for confirmation before applying it. Rerun to resume. `cqlmatic setup --json` reads saved status without prompting, probing clients, reading client configuration, or creating state. Automation uses `--request FILE --json` and `cqlmatic --schema setup`; see [contracts](CONTRACTS.md) for framing and [the setup examples](../examples/setup/README.md) for request files.

The initial native adapters are **Codex CLI** and **VS Code**, on Windows and Linux. VS Code CIMD requires version 1.106 or newer, as documented in its [release notes](https://code.visualstudio.com/updates/v1_106); Codex capabilities are checked through installed command help. Install and sign into your assistant first. CQLmatic does not install a client or grant Falcon permissions. Missing clients/capabilities produce guided next steps. Other clients can use the endpoint/authentication information below and their own MCP setup UI.

## Install on Windows

Download `cqlmatic-v0.1.0-windows-x86_64.zip` and `SHA256SUMS` from the official [v0.1.0 release](https://github.com/mychaelconnolly/cqlmatic/releases/tag/v0.1.0). Use the x64 ZIP only on an x64 Windows system. The prebuilt executable needs neither Rust nor C++ build tools. Source builds require the toolchain described in [development](DEVELOPMENT.md#native-windows).

Before extraction, compare `Get-FileHash -Algorithm SHA256` for the ZIP with the exact filename's entry in `SHA256SUMS`. Stop if the entry is missing, duplicated or different. Extract into a new version-specific directory under your user profile, such as `%LOCALAPPDATA%\Programs\CQLmatic\v0.1.0`; do not overwrite an existing installation. The archive's `cqlmatic-v0.1.0` directory contains `cqlmatic.exe`, guides, examples and licenses.

Run the extracted executable directly:

```powershell
$Cqlmatic = Join-Path $env:LOCALAPPDATA 'Programs/CQLmatic/v0.1.0/cqlmatic-v0.1.0/cqlmatic.exe'
& $Cqlmatic --version
& $Cqlmatic --help
& $Cqlmatic setup
```

An agent installing the release should verify the OS/architecture, exact official release URL, checksum and executable version before continuing. Installation does not need administrator access or a global PATH change. Keep authentication and client configuration in the reviewed setup flow below. The Windows executable is unsigned; use the official release and verify its checksum rather than treating a download warning as proof of authenticity.

## Choose a connection

1. **CIMD (preferred).** Use it when the installed client supports Client ID Metadata Documents and CrowdStrike has allowlisted that client's metadata identity. The wizard asks you to confirm the metadata/allowlist prerequisite. Your Falcon administrator may need to arrange allowlisting. CIMD avoids manually creating a separate MCP OAuth application; browser login and consent are still required.
2. **Manual MCP registration.** Register the AI application in Falcon using the exact callback URI required by your client. Supply its nonsecret client ID through the wizard. If a secret is required, enter it only through the client's protected native flow. Do not put it in request JSON, command arguments, or CQLmatic state. CrowdStrike does not support dynamic client registration as a fallback.
3. **Advanced service/API-client connection.** Follow the selected client's supported bearer-token or gateway flow. This branch remains pending when the native adapter cannot perform it safely. API client credentials differ from MCP OAuth application registration. CQLmatic does not install a general MCP bearer-token service for you.

| Falcon cloud | Remote MCP endpoint |
| --- | --- |
| US-1 | `https://mcp.crowdstrike.com/v1` |
| US-2 | `https://mcp.us-2.crowdstrike.com/v1` |
| EU-1 | `https://mcp.eu-1.crowdstrike.com/v1` |

Use the cloud containing your account. MCP endpoints and Falcon API base URLs differ. Reconfirm callback and metadata requirements against current client documentation; they vary by client/version.

## Permissions and verification

For user OAuth, assign the appropriate CrowdStrike MCP role (for example `CrowdStrikeMCPUserReadOnly`) and underlying Falcon roles for the tools you need. An MCP role alone does not grant access to all telemetry. Tenant/member scope, consent, network policies, and selected tools also affect visibility. Prefer read-only access.

Setup distinguishes:

- **Configured:** a native configuration action completed or the same endpoint was already registered.
- **Authenticated:** a matching saved host report states that authentication succeeded. User confirmation is recorded separately and leaves verification pending until the report is supplied.
- **Tool discovery verified:** a saved host report matches the selected client/server/endpoint and includes the required search tool. This is caller-supplied evidence with a recorded hash, not a live transport attestation or telemetry search.

After consent, ask your assistant to refresh its MCP tools. Save its tool-discovery report using the bundled schema/example and import it through setup. A configured server may still need authentication or a client restart. Setup never treats a config entry as proof of authentication.

Native commands may open a browser. Some Codex versions start OAuth during server registration; review that side effect in the preview. Applying a plan containing an interactive command requires a terminal and non-JSON mode. Applying that plan with `--json`, or without a terminal, returns the global `SETUP_INTERACTIVE_REQUIRED` error and exit 2 before running the command; it does not produce a successful pending setup result. Noninteractive commands can be applied through JSON requests when the adapter emits them.

Unrelated configuration is preserved. Name collisions, changed config fingerprints/executables, or stale previews require a fresh preview. Backups and state are local; CQLmatic's setup contract accepts no client secret. The native client stores its OAuth material according to its own security model.

Codex's native list/get output may hide OAuth registration details. An existing matching URL alone is insufficient to reuse a registration: setup requires matching local provenance from its own reviewed registration, or leaves the step pending with guidance. It does not overwrite the existing server. For manual Codex reauthentication, versions without a client-ID option on `mcp login` require the native guided flow; CQLmatic does not assume an ID selected for registration applies to every later login. These checks do not prevent inventory through an already authorized MCP client.

## Actions and resumable state

Each strict v1 request contains `schema_version: 1`, optional `id`, and `action.type`. Unknown fields are rejected, including secret and caller-selected executable fields. A setup result contains `state`, `next_actions`, a host-agent `handoff` when a client is selected, source references, and diagnostics. Read the phase and evidence rather than inferring success from the presence of a handoff.

| Action | Inputs and effect |
| --- | --- |
| `status` | No action fields. Reports saved state through the request path. Unlike no-request `setup --json`, a status **request** uses the protected writer path and can create/save state. |
| `detect` | No action fields. Probes installed supported clients and records canonical executable, executable hash, version, capabilities, and capability hash. It installs nothing. |
| `select` | `selection` contains `client` (`codex` or `vs-code`), `cloud` (`us1`, `us2`, `eu1`), `server_name`, and `registration`. Clears the prior plan, configuration assertion, and authentication/discovery evidence. |
| `preview` | `operation` is `configure` or `authenticate`. Inspects the current native baseline and derives exact commands and a confirmation hash. Does not run those commands. |
| `apply` | `confirmation_sha256` must match the current reviewed preview. Rechecks executable/capabilities and configuration before native execution. |
| `confirm-authentication` | No action fields. Requires configured state; records user confirmation and remains `authentication-required`. |
| `import-discovery` | `file` points to a saved strict host report. Rechecks unchanged native configuration and binds the report as described below. |
| `acknowledge-pending` | `confirmation_sha256` identifies the pending plan inspected by the operator. Clears configuration/authentication assertions and requires a fresh preview. |
| `cancel` | No action fields. Saves progress and invalidates an ordinary preview; an unresolved `command-pending` phase remains pending. |

Registration input is one of:

- `cimd`: exact published `metadata_url` and `allowlist_confirmed: true`. This is an operator confirmation of the prerequisite, not a local allowlist probe. VS Code's supported identity is `https://vscode.dev/oauth/client-metadata.json`; Codex's accepted metadata identity depends on the current client and must be confirmed before selection.
- `manual`: nonsecret `client_id` and the native client's exact `callback_url` (HTTPS or HTTP loopback, without credentials/fragments). Secrets go only through the client's protected flow.
- `api-client`: absolute `credentials_file` reference. Setup records the path without reading it; this selection remains `advanced-pending` with native service/plugin guidance.

`server_name` is a 1–64 character ASCII identifier using letters, digits, underscores or hyphens, starting with a letter or digit. A selection moves to `selected`, `installation-required` when the selected client is missing, or `advanced-pending` for service mode. Missing capabilities and unsafe reuse require guided action; they never trigger silent dynamic registration.

A normal native path is `new` → `selected` → `review-required` → `command-pending` → `authentication-required`, followed by importing a matching report into `authenticated` or `tool-discovery-verified`. Missing installation, service mode, cancellation, and incomplete native work have separate phases. `authenticated` is reached by a matching saved authenticated report lacking the required available search tool; user confirmation alone does not reach it. Setup returns exit 0 only in `tool-discovery-verified`, exit 1 for other ordinary phases, and exit 2 for global input/I/O/action errors. A successful detect, selection, or preview therefore normally exits 1.

## Preview, apply, and interrupted commands

Review the preview's absolute executable, argv, `interactive` flags, native config target and original fingerprint, protected backup path, changes, state revision, and `confirmation_sha256`. The hash binds the entire plan, including selection and observed client capabilities. Apply reconstructs commands from trusted adapter data; editing stored argv does not create a supported command interface. A changed executable, capability, state revision, or configuration requires a new preview. Colliding server names are refused, and unrelated configuration is preserved.

Configure backs up an existing configuration before a native change. Before starting commands, setup durably records `command-pending`. A failure, timeout, interruption, or failed postcondition can leave partial native changes. Inspect the client and protected backup, then submit `acknowledge-pending` with that pending plan's hash. Preview the inspected current baseline before another apply. Setup does not automatically retry, force an overwrite, restore a backup, or claim rollback. Cancellation does not clear this inspection requirement.

A successful native command is followed by a configuration postcondition check. It establishes configured state only; even an authentication command's exit status does not attest authentication. An already matching server can skip an add command, subject to registration-provenance checks. Codex's receipt binds a reviewed addition to unchanged local configuration; it is not an independent inspection of hidden OAuth identity.

## Saved host tool-discovery report

After native login/consent, ask the selected host agent to discover available tools and save a report matching [the synthetic report shape](../examples/setup/tool-discovery.json). It contains `schema_version`, `client`, `server_name`, canonical `endpoint`, `authenticated`, and `tools` entries with exact `name` and `available` values. The report must identify the selected client/server/cloud endpoint, assert authentication, contain no duplicate tool names, and include exact `falcon_search_ngsiem` with `available: true` to reach `tool-discovery-verified`. A tool name mentioned in prose, a prefixed name, or an unavailable entry is insufficient.

Import records the saved bytes' SHA-256 and `saved-host-tool-discovery` authentication evidence. An otherwise valid authenticated report without that exact available tool reaches `authenticated` and exits 1. This is caller-supplied evidence, not a live transport attestation, permission test, telemetry query, or completeness claim. Keep the actual report private; never import the bundled synthetic report as real evidence. Use the handoff to continue into [inventory](INVENTORY.md).

The handoff's `search-all` guidance is not search authorization. Inventory's built-in scout actually fixes that target independently of the saved environment view. Confirm authorization for search-all before relaying it; otherwise leave scouting pending and use selected-source discovery on the authorized view. Later discovery and query execution must retain their confirmed target.

## Parser inventory is a separate connection

CQLmatic's MCP search workflow does not retrieve the parser catalog. Create a Falcon API client with **NGSIEM Parsers: READ** for catalog and definition/template reads. Parser write permissions are unnecessary. This client supplements MCP scouting and field measurement.

Inventory reads catalog pages and exact-ID definitions. For an individual native YAML template, the lower-level `cqlmatic env parsers --environment PATH_TO_PRIVATE_STATE/environment.json --credentials-file PATH_TO_PROTECTED_CREDENTIALS.json --id PARSER_ID --template` command is also available. Keep its output private. Template retrieval alone does not establish a source binding or measured field presence.

Store credentials in a protected local file outside repositories and shared/synced delivery folders. CQLmatic accepts JSON or supported labeled text:

```json
{
  "client_id": "YOUR_API_CLIENT_ID",
  "client_secret": "YOUR_API_CLIENT_SECRET",
  "base_url": "https://api.crowdstrike.com"
}
```

API base URLs: `https://api.crowdstrike.com` (US-1), `https://api.us-2.crowdstrike.com` (US-2), or `https://api.eu-1.crowdstrike.com` (EU-1). Use mode `0600` on Unix and a private user ACL on Windows. Put only the credential **file path** in inventory requests. Lower-level API commands also support environment variables; see [environment details](ENVIRONMENT.md). Keep secrets out of chat, logs, saved requests, and committed config.

MCP user identity and parser API identity must describe the same tenant/cloud/view. `cqlmatic env identity --cloud us1 --view search-all --credentials-file PATH_TO_PROTECTED_CREDENTIALS.json` derives a local API-client fingerprint without authenticating. Do not substitute that fingerprint for a real CID in an existing MCP target. Multi-tenant use requires an explicitly matching member CID; confirm it with your administrator.

Offline writing, checking, catalog lookup, and status need no Falcon key. MCP discovery uses the client's OAuth. Full-map completion additionally requires parser inventory/review and catalog reconciliation. Native sources can receive an explicit supported native-parser classification. Your assistant uses its own model account; CQLmatic needs no model API key.

## Local state and recovery

Windows defaults are `%LOCALAPPDATA%\cqlmatic\setup\state.json` and `%LOCALAPPDATA%\cqlmatic\inventory\state.json`. Linux setup uses `$XDG_STATE_HOME/cqlmatic/setup/state.json` or `~/.local/state/cqlmatic/setup/state.json`; inventory uses `$XDG_DATA_HOME/cqlmatic/inventory/state.json` or `~/.local/share/cqlmatic/inventory/state.json`. On macOS, setup uses the same `$XDG_STATE_HOME`/`~/.local/state` rule; inventory defaults to `~/Library/Application Support/cqlmatic/inventory/state.json` when neither `LOCALAPPDATA` nor `XDG_DATA_HOME` is set. Use `--state PATH_TO_PRIVATE_STATE/state.json` for separate environments. Both workflows require a dedicated private directory and refuse an existing broadly accessible directory. Packs, evidence, and receipts can contain tenant information: keep them private too.

For expired authentication, use native login, refresh the host tool-discovery report, and import it again. Saved status does not perform authentication renewal. For 401, confirm cloud, authentication, and renewal. For 403, check Falcon roles, API scopes, tenant/member binding, consent, and egress allowlists. OAuth success alone does not grant parser-read or search permission. Narrow an inconclusive search before retrying. See [troubleshooting](TROUBLESHOOTING.md) for recovery by diagnostic.

References: [CrowdStrike NGSIEM API](https://developer.crowdstrike.com/api-reference/collections/ngsiem/), [VS Code MCP configuration](https://code.visualstudio.com/docs/agents/reference/mcp-configuration), and installed client `mcp --help`.
