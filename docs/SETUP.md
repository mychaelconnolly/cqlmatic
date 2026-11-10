# Setup and authentication

With a [supported, patched Node.js release](../README.md#try-v012) on macOS Apple silicon or Linux x86_64, use `npx cqlmatic@0.1.2 setup` from outside a CQLmatic source checkout. Use your source-built executable inside that checkout: npm can select the local package, which has no bundled executable. The npm package runs the native executable on demand without installing a global command. The guides use `cqlmatic` as shorthand for that pinned command or your standalone executable. Linux requires glibc 2.34 or newer and `libgcc_s.so.1`. Windows x64 uses the [release ZIP](#install-on-windows); other architectures use a [source build](DEVELOPMENT.md).

Run `npx` in a foreground terminal. For unattended process supervision, use the native executable directly: npm can add stderr notices, and cancellation directed only at its parent PID may not reach the command. See [input and output rules](../USAGE.md#input-and-output-rules).

Run `cqlmatic setup` in a terminal. Choose an agent and Falcon cloud. The named native adapters preview the exact host configuration action when they can perform it safely and ask for confirmation. Codex, VS Code Local and Claude Code require local executable detection; the shared Copilot configuration adapter does not. The `external` selection tracks a guided path for other capable agents. Rerun to resume. `cqlmatic setup --json` reads saved status without prompting, probing clients, reading client configuration, or creating state. Automation uses `--request FILE --json` and `cqlmatic --schema setup`; see [contracts](CONTRACTS.md) and [setup examples](../examples/setup/README.md).

Invalid selections, unsupported host capabilities, stale confirmations and rejected report files return to the interactive menu after reporting the action error. State protection, state integrity, persistence and terminal I/O failures stop the wizard. A failed native action still requires inspection when its saved phase is `command-pending`.

Native setup covers **Codex CLI**, **VS Code Local**, **Claude Code**, **GitHub Copilot CLI**, **GitHub Copilot app**, and the **VS Code Copilot harness**. [Codex desktop, CLI and IDE share MCP configuration](https://learn.chatgpt.com/docs/extend/mcp?surface=cli) when running in the same environment. Copilot CLI, app and VS Code Copilot share a Copilot user configuration when running in the same environment. The selected host still owns OAuth, consent, tool selection and saved results. CQLmatic does not install an agent or grant Falcon permissions.

## Install on Windows

Download `cqlmatic-v0.1.2-windows-x86_64.zip` and `SHA256SUMS` from the official [v0.1.2 release](https://github.com/mychaelconnolly/cqlmatic/releases/tag/v0.1.2). Use the x64 ZIP only on an x64 Windows system. The prebuilt executable needs neither Rust nor C++ build tools. Source builds require the toolchain described in [development](DEVELOPMENT.md#native-windows).

Before extraction, compare `Get-FileHash -Algorithm SHA256` for the ZIP with the exact filename's entry in `SHA256SUMS`. Stop if the entry is missing, duplicated or different. Extract into a new version-specific directory under your user profile, such as `%LOCALAPPDATA%\Programs\CQLmatic\v0.1.2`; do not overwrite an existing installation. The archive's `cqlmatic-v0.1.2` directory contains `cqlmatic.exe`, guides, examples and licenses.

Run the extracted executable directly:

```powershell
$Cqlmatic = Join-Path $env:LOCALAPPDATA 'Programs/CQLmatic/v0.1.2/cqlmatic-v0.1.2/cqlmatic.exe'
& $Cqlmatic --version
& $Cqlmatic --help
& $Cqlmatic setup
```

An agent installing the release should verify the OS/architecture, exact official release URL, checksum and executable version before continuing. Installation does not need administrator access or a global PATH change. Keep authentication and client configuration in the reviewed setup flow below. The Windows executable is unsigned; use the official release and verify its checksum rather than treating a download warning as proof of authenticity.

## Choose a connection

1. **Claude Code CIMD.** Claude Code can discover its Client ID Metadata Document for CrowdStrike MCP. Confirm that Falcon currently allows the exact `https://claude.ai/oauth/claude-code-client-metadata` identity before choosing this mode. Setup configures the HTTP server without a client ID; Claude handles browser sign-in and consent. Setup does not invoke login or force Claude's OAuth registration choice. If the effective session uses another identity or attempts DCR, leave setup pending and inspect the host.
2. **Manual MCP registration for the other named hosts.** Register the AI application in Falcon for the host's actual callback behavior and supply its nonsecret client ID to setup. The native adapter places that ID in Codex, VS Code Local, or a new Copilot user configuration before connection. For an existing Copilot file, add the exact entry through the host's configuration flow and let setup verify it; setup will not replace a file that Copilot may update concurrently. The `callback_url` selection field records the operator's expected callback; these adapters do not set a callback override. Confirm the redirect used by the installed host/version in Falcon and complete browser consent there. If a confidential client needs a secret, enter it only through the host's protected flow. Do not put it in setup requests, command arguments, or state. An unconfigured ID can trigger unsupported DCR.
3. **Advanced service/API-client connection.** Follow the selected client's supported bearer-token or gateway flow. This branch remains pending when the native adapter cannot perform it safely. API client credentials differ from MCP OAuth application registration. CQLmatic does not install a general MCP bearer-token service for you.

| Falcon cloud | Remote MCP endpoint |
| --- | --- |
| US-1 | `https://mcp.crowdstrike.com/v1` |
| US-2 | `https://mcp.us-2.crowdstrike.com/v1` |
| EU-1 | `https://mcp.eu-1.crowdstrike.com/v1` |

Use the cloud containing your account. MCP endpoints and Falcon API base URLs differ. Reconfirm callback and metadata requirements against current client documentation; they vary by client/version.

## Native hosts and effective configuration

Select a named host with `cimd` for Claude Code or `manual` for a host requiring a registered client ID. Preview and apply the reviewed configuration. CQLmatic checks the local target and known name collisions; the chosen session can still receive managed, plugin, workspace or command-line overrides. **Before connecting**, inspect the effective server and OAuth client ID in that session. After sign-in, record those values in its [saved report](#saved-host-tool-discovery-report). A matching user file alone does not establish the session's identity.

| Host | Native target and effective check |
| --- | --- |
| [Codex](https://learn.chatgpt.com/docs/extend/mcp?surface=cli) | The detected Codex CLI adds the server to its shared `config.toml` (or `CODEX_HOME`), used by desktop, CLI and IDE in the same environment. Setup binds a fresh add to its reviewed registration because `codex mcp list` hides some OAuth fields. Confirm the selected client ID and callback behavior in Codex before sign-in. |
| [VS Code Local harness](https://code.visualstudio.com/docs/agents/reference/mcp-configuration) | The detected `code --add-mcp` command targets the default user profile's `mcp.json` with top-level `servers` and `oauth.clientId`. Portable/custom profiles and remote extension hosts need host review. Select the **Local** harness and check its effective entry. |
| [Claude Code](https://code.claude.com/docs/en/mcp) | The detected `claude mcp add --scope user --transport http` command writes the selected endpoint to `~/.claude.json`, or `CLAUDE_CONFIG_DIR/.claude.json`, without a client ID. Claude discovers its allowlisted CIMD identity. Local scope outranks project, then user; managed servers can outrank all three, and per-session `--mcp-config`/strict flags can change the result. Setup refuses known local/project and on-disk managed shadows, but server-managed policy can still change the session. Check `/mcp` before sign-in. Claude Code v2.1.229 used a `127.0.0.1` callback; v2.1.231 restored `localhost`. Inspect the installed version and actual callback if registration or consent fails. |
| [GitHub Copilot CLI](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference) | When the user file is absent, setup atomically creates a private HTTP entry with `oauthClientId` and only `falcon_search_ngsiem` enabled at `$COPILOT_HOME/mcp-config.json`, or `~/.copilot/mcp-config.json`. If the file exists, setup refuses to replace it: add that entry through Copilot's configuration flow, then preview again to verify it. `copilot mcp add` has no client-ID flag. Copilot precedence is session override, plugin, workspace/repository, then user file. Inspect the effective entry in the actual session before connecting. `copilot` on Windows and other supported systems and [`gh copilot`](https://cli.github.com/manual/gh_copilot) launch the **same CLI**; `gh copilot` may download it if absent. |
| [GitHub Copilot app](https://docs.github.com/en/copilot/how-tos/github-copilot-app/customize-github-copilot-app) | Uses the same Copilot user file when the app runs in that environment; repository and CLI servers are also available. Select the configured server in Customize > MCP and check its effective `oauthClientId` before connecting. A separate app/session source may override the file. |
| [VS Code Copilot harness](https://code.visualstudio.com/docs/agent-customization/mcp-servers) | Uses the same Copilot Global file when VS Code runs in that environment. Select the **Copilot** harness and check the effective entry. Portable workspace `.mcp.json` uses top-level `mcpServers` and can take precedence. This differs from the VS Code **Local** adapter above. |

For another capable agent choose `external` with `registration: {"mode":"host-managed"}` and a short `host_id`. CQLmatic tracks the host and binds its report, but cannot safely write an unknown configuration format. The execution host must run CQLmatic, access protected local files, connect to the authorized MCP endpoint, and save exact MCP results. Remote sessions qualify only when those capabilities exist there. Confirm supported host-managed authentication before importing a report. External reports may omit `effective_oauth_client_id` and `effective_config_source`; include those operator-supplied fields only when known. The next actions include the selected host ID for exact report binding. Keep configuration, tokens and reports private.

## Permissions and verification

For user OAuth, assign the appropriate CrowdStrike MCP role (for example `CrowdStrikeMCPUserReadOnly`) and underlying Falcon roles for the tools you need. An MCP role alone does not grant access to all telemetry. Tenant/member scope, consent, network policies, and selected tools also affect visibility. Prefer read-only access.

Setup distinguishes:

- **Configured:** a named native adapter observed its expected local entry, or an imported `external` report states a connected server. This does not prove that a running session selected that entry.
- **Authenticated:** a bound saved report states that the chosen host authenticated. New Claude/Copilot reports must also state the selected session's effective OAuth client ID and configuration source; both remain caller-supplied evidence. User confirmation alone leaves verification pending.
- **Tool discovery verified:** a bound authenticated report includes exact available `falcon_search_ngsiem`. This is caller-supplied evidence with a recorded hash, not an independent transport, OAuth, permission or telemetry check.

After consent, ask the chosen host to refresh its MCP tools and save its tool-discovery report using the bundled schema/examples. Import that report through setup. A configured server may still need authentication or a client restart. An `external` report is accepted without local configuration inspection. For named native hosts, setup checks that the selected entry remains present; Claude and Copilot checks fingerprint that entry rather than unrelated settings in the same file.

Native commands may open a browser. Some Codex versions start OAuth during server registration; review that side effect in the preview. Applying a plan containing an interactive command requires a terminal and non-JSON mode. Applying that plan with `--json`, or without a terminal, returns the global `SETUP_INTERACTIVE_REQUIRED` error and exit 2 before running the command; it does not produce a successful pending setup result. Noninteractive commands can be applied through JSON requests when the adapter emits them.

Unrelated configuration is preserved. Copilot's existing user file is never rewritten by setup; its selected entry can still be verified after a host-side addition. Creating a new Copilot file stages it in protected setup storage and requires the storage and Copilot user file to be on the same volume for atomic activation. Otherwise create the entry through Copilot and preview again. Name collisions, changed config fingerprints/executables, or stale previews require a fresh preview. Backups and state are local; CQLmatic's setup contract accepts no client secret. The native client stores its OAuth material according to its own security model.

Codex's native list/get output may hide OAuth registration details. An existing matching URL alone is insufficient to reuse a registration: setup requires matching local provenance from its own reviewed registration, or leaves the step pending with guidance. It does not overwrite the existing server. For manual Codex reauthentication, versions without a client-ID option on `mcp login` require the native guided flow; CQLmatic does not assume an ID selected for registration applies to every later login. These checks do not prevent inventory through an already authorized MCP client.

## Actions and resumable state

Each strict v1 request contains `schema_version: 1`, optional `id`, and `action.type`. Unknown fields are rejected, including secret and caller-selected executable fields. A setup result contains `state`, `next_actions`, a host-agent `handoff` when a client is selected, source references, and diagnostics. Read the phase and evidence rather than inferring success from the presence of a handoff.

| Action | Inputs and effect |
| --- | --- |
| `status` | No action fields. Reports saved state through the request path. Unlike no-request `setup --json`, a status **request** uses the protected writer path and can create/save state. |
| `detect` | No action fields. Probes Codex CLI, VS Code Local and Claude Code and records executable/capability fingerprints. A failed client probe or fingerprint does not discard other detected clients. Copilot configuration needs no executable probe. Installs nothing and does not demote `external`. |
| `select` | `selection` contains `client` (`codex`, `vs-code`, `claude-code`, `copilot-cli`, `copilot-app`, `copilot-vscode`, or `external`), `cloud` (`us1`, `us2`, `eu1`), `server_name`, and `registration`. `external` alone requires `host_id` (1–64 ASCII identifier). Clears the prior plan, configuration assertion, and authentication/discovery evidence. |
| `preview` | Named native adapters only. `operation` is `configure` or `authenticate`. Inspects the current baseline and derives the command or new Copilot file creation, backup, and confirmation hash without connecting. An existing Copilot file without the selected entry returns `SETUP_EXISTING_CONFIG` for host-side addition. |
| `apply` | Named native adapters only. `confirmation_sha256` must match the reviewed preview. Rechecks executable/capabilities where needed and configuration before the command or atomic write. |
| `confirm-authentication` | Named native adapters only. Requires configured state; records user confirmation and remains `authentication-required`. |
| `import-discovery` | `file` points to a saved strict host report. Named native adapters recheck the selected entry; `external` binds the report without local configuration inspection. |
| `acknowledge-pending` | `confirmation_sha256` identifies the pending plan inspected by the operator. Clears configuration/authentication assertions and requires a fresh preview. |
| `cancel` | No action fields. Saves progress and invalidates an ordinary preview; an unresolved `command-pending` phase remains pending. |

Registration input is one of:

- `cimd`: exact published `metadata_url` and `allowlist_confirmed: true`. Claude Code's accepted identity is `https://claude.ai/oauth/claude-code-client-metadata`; this is an operator confirmation, not a local allowlist probe. Existing Codex/VS Code CIMD selections remain readable and can be used only with separate confirmation of current Falcon allowlisting; manual registration is the supported default for those hosts.
- `manual`: nonsecret `client_id` and expected `callback_url` (HTTPS or HTTP loopback, without credentials/fragments). The callback is recorded and shown in preview, not configured by these adapters. Confirm the host's actual callback and Falcon registration; secrets go only through the host's protected flow. Claude Code's native adapter uses CIMD instead.
- `api-client`: absolute `credentials_file` reference. Setup records the path without reading it; this selection remains `advanced-pending` with native service/plugin guidance.
- `host-managed`: no credential fields. Available only for `external`; native setup actions are unavailable.

`server_name` and external `host_id` are 1–64 character ASCII identifiers using letters, digits, underscores or hyphens, starting with a letter or digit. `external` and the Copilot surfaces move to `selected` without executable detection. Codex, VS Code Local and Claude Code can enter `installation-required` when their executable is missing. Service mode remains `advanced-pending`. Missing capabilities and unsafe reuse require guided action; they never trigger a setup-managed DCR fallback.

A normal native path is `new` → `selected` → `review-required` → `command-pending` → `authentication-required`, followed by importing a matching report into `authenticated` or `tool-discovery-verified`. Missing installation, service mode, cancellation, and incomplete native work have separate phases. `authenticated` is reached by a matching saved authenticated report lacking the required available search tool; user confirmation alone does not reach it. Setup returns exit 0 only in `tool-discovery-verified`, exit 1 for other ordinary phases, and exit 2 for global input/I/O/action errors. A successful detect, selection, or preview therefore normally exits 1.

The `external` path is `new` → `selected` → `authenticated` or `tool-discovery-verified` after importing a report. If the host cannot complete supported registration and sign-in, it stays `selected`. A valid report without the search tool remains `authenticated` with exit 1; a malformed or mismatched report is rejected without changing saved state. Existing v1 Codex/VS Code native state remains readable; older binaries may reject new selections, so use a separate `--state` path per host workflow.

## Preview, apply, and interrupted commands

Review the preview's executable/argv when present, Copilot config write hash when present, `interactive` flags, config target and original fingerprint, protected backup path, changes, state revision, and `confirmation_sha256`. The Copilot write hash covers the exact new file bytes, including its trailing newline. The confirmation hash binds the plan, selection, and observed executable capabilities where applicable. Apply reconstructs commands or Copilot file bytes from trusted selection data; editing stored plan fields is not an action interface. A changed executable, capability, revision, or config baseline requires a new preview. Name collisions are refused and unrelated configuration is preserved.

Executable fingerprints stream SHA-256 through a fixed buffer with a 1 GiB byte cap checked before and during reading. Setup requires a regular file with stable metadata and file identity across capability detection and hashing. Preview and apply recheck only the selected executable; a failure identifies that host and blocks its native action.

Configure backs up an existing configuration before a native change. Before a command or file write, setup durably records `command-pending`. A failure, timeout, interruption, or failed postcondition can leave partial changes. Inspect the host and protected backup, then submit `acknowledge-pending` with that pending plan's hash. Preview the inspected current baseline before another apply. Setup does not retry, force an overwrite, restore a backup, or claim rollback. Cancellation does not clear this inspection requirement.

A successful native command or Copilot file write is followed by a configuration postcondition check. It establishes local configured state only; even an authentication command's exit status does not attest authentication. An already matching server can skip an add/write, subject to Codex's registration-provenance check. Its receipt binds a reviewed addition to unchanged local configuration; it does not inspect hidden OAuth identity.

## Saved host tool-discovery report

After host sign-in/consent, ask the selected agent to discover available tools and save a report matching the [Codex/VS Code](../examples/setup/tool-discovery.json), [Claude](../examples/setup/tool-discovery-claude.json), [Copilot](../examples/setup/tool-discovery-copilot.json), or [external](../examples/setup/tool-discovery-host.json) synthetic shape. It contains `schema_version`, `client`, `server_name`, canonical `endpoint`, `authenticated`, and `tools` with exact `name` and `available` values. New Claude/Copilot reports also require `effective_oauth_client_id` matching the selected identity and a nonempty `effective_config_source` observed in the selected host session. Codex/VS Code legacy reports may omit those fields, but a supplied effective client ID must not contradict the selection. `external` requires the selected `host_id`; named hosts omit it. The report must match the selected client/host ID/server/cloud endpoint and contain no duplicate tool names. Exact available `falcon_search_ngsiem` reaches `tool-discovery-verified`.

Import records the saved bytes' SHA-256 and `saved-host-tool-discovery` authentication evidence. A valid authenticated report without the exact available tool reaches `authenticated` and exits 1. For `external`, import also marks `configured: true` with no local configuration fingerprint; this means only that the report asserts a connected server. All reports, including their effective-identity fields, are caller-supplied evidence rather than independent proof of transport, OAuth, permission, telemetry or completeness. Keep actual reports private; never import bundled synthetic reports as real evidence. Use the handoff to continue into [inventory](INVENTORY.md).

Additional available tools produce an advisory to review host permissions and restrict unneeded tools. Their names do not establish read or write behavior, and their presence does not change whether the exact search tool satisfies setup discovery.

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

Windows defaults are `%LOCALAPPDATA%\cqlmatic\setup\state.json` and `%LOCALAPPDATA%\cqlmatic\inventory\state.json`. Linux setup uses `$XDG_STATE_HOME/cqlmatic/setup/state.json` or `~/.local/state/cqlmatic/setup/state.json`; inventory uses `$XDG_DATA_HOME/cqlmatic/inventory/state.json` or `~/.local/share/cqlmatic/inventory/state.json`. On macOS, setup uses the same `$XDG_STATE_HOME`/`~/.local/state` rule; inventory defaults to `~/Library/Application Support/cqlmatic/inventory/state.json` when neither `LOCALAPPDATA` nor `XDG_DATA_HOME` is set. Use `--state PATH_TO_PRIVATE_STATE/state.json` for separate environments. Both workflows require a dedicated private directory whose parent path cannot be replaced by another user; they reject unsafe Unix modes, macOS ACL grants, or unsafe Windows ACL rights on that path. If an existing path was previously shared, choose fresh private storage because permission changes cannot revoke access from processes that already hold a handle. Packs, evidence, and receipts can contain tenant information: keep them private too.

On Windows, setup checks existing ancestry before creating missing directories, then checks the private directory and ancestry again. ACL failures identify the first failing path, principal SID and right when the fixed helper returns a bounded diagnostic. Setup does not change permissions on existing directories or delete a path after a failed protection check.

For expired authentication, use native login, refresh the host tool-discovery report, and import it again. Saved status does not perform authentication renewal. For 401, confirm cloud, authentication, and renewal. For 403, check Falcon roles, API scopes, tenant/member binding, consent, and egress allowlists. OAuth success alone does not grant parser-read or search permission. Narrow an inconclusive search before retrying. See [troubleshooting](TROUBLESHOOTING.md) for recovery by diagnostic.

References: [CrowdStrike NGSIEM API](https://developer.crowdstrike.com/api-reference/collections/ngsiem/), [Claude Code MCP](https://code.claude.com/docs/en/mcp), [GitHub Copilot CLI MCP](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference), [VS Code MCP configuration](https://code.visualstudio.com/docs/agent-customization/mcp-servers), and installed client `mcp --help`.
