# Agent setup examples

Start interactively with `cqlmatic setup`. `--json` without a request reports saved progress and never waits for input. A fresh status has no environment or credentials.

The JSON files here are synthetic contract examples, not evidence of a configured or authenticated client. Use a new dedicated private directory for `--state`; omitting it uses OS-local CQLmatic setup storage.

```text
cqlmatic setup --request examples/setup/detect.request.json --json
cqlmatic setup --request examples/setup/select.request.json --json
cqlmatic setup --request examples/setup/select-claude.request.json --json
cqlmatic setup --request examples/setup/select-copilot.request.json --json
cqlmatic setup --request examples/setup/preview.request.json --json
cqlmatic setup --request examples/setup/select-host.request.json --json
```

`select.request.json` illustrates a VS Code Local manual client ID. `select-claude.request.json` illustrates Claude Code CIMD; set `allowlist_confirmed` only after confirming that exact identity is currently allowed in Falcon. `select-copilot.request.json` illustrates the shared Copilot native adapter; switch its `client` to `copilot-app` or `copilot-vscode` for those surfaces. Replace all synthetic IDs and callbacks before use. Local detection cannot establish tenant access.

`select-host.request.json` illustrates the guided path for an arbitrary `external` agent with a `host_id`. Replace `example-agent` with a 1–64 character ASCII ID and use the same value in its saved report. Only `external` uses `{"mode":"host-managed"}`. `copilot` on Windows and `gh copilot` launch the same Copilot CLI, so both select `copilot-cli`. See [native host setup](../../docs/SETUP.md#native-hosts-and-effective-configuration) for effective configuration and OAuth identity checks.

For Codex, choose `"client": "codex"` with a registered manual client ID. Existing CIMD selections remain readable, but current CrowdStrike support should be confirmed for the exact identity before choosing that route. Setup never silently chooses DCR.

A preview names the executable and argv where used, or the hash of a new Copilot file, plus the config target, original fingerprint, protected backup path and setup-state changes. Setup creates a Copilot user file only when it is absent. If one exists without the selected entry, add it through Copilot's configuration flow and preview again; setup verifies the entry without replacing the file. Review the preview. To apply, submit `"action": {"type":"apply","confirmation_sha256":"<the exact reviewed 64-character hash>"}`. Do not reuse a hash after any selection, binary, capability, or configuration change. Codex add can immediately start browser consent, so applying that command requires an interactive terminal. An interactive plan in JSON mode or without a TTY returns `SETUP_INTERACTIVE_REQUIRED` and exit 2 before running it.

Manual registration uses `"registration":{"mode":"manual","client_id":"synthetic-generated-id","callback_url":"http://127.0.0.1:1234/callback"}`. Replace both values with the admin-generated MCP client ID and the callback expected for the installed host. The callback is recorded, not configured by setup; confirm what the host actually sends before relying on Falcon registration. Optional secrets go only through the host's protected UI. Never put secrets in a request, CLI argument, setup state, or source control.

Advanced service mode uses `"registration":{"mode":"api-client","credentials_file":"<absolute protected file path>"}`. Setup records that reference without opening it and remains advanced-pending until a supported native client/plugin is configured. CQLmatic does not perform OAuth or MCP transport.

After host browser sign-in, `"action":{"type":"confirm-authentication"}` records user confirmation only. It is unavailable for `external`, as are native `preview` and `apply`. Configuration, command exit status and MCP list output do not attest authentication. Ask the chosen agent to save an actual report in the strict shape shown by `tool-discovery.json`, `tool-discovery-claude.json`, `tool-discovery-copilot.json`, or `tool-discovery-host.json`. Claude/Copilot reports must state the selected session's effective OAuth client ID and configuration source. Never import these synthetic files as real evidence. Import with `"action":{"type":"import-discovery","file":"<saved report path>"}`. Client, optional external host ID, server and canonical endpoint must match. Exact available `falcon_search_ngsiem` reaches verified discovery; a bound authenticated report without it remains authenticated with exit 1. Reports are caller-supplied, not independent transport or OAuth attestation, and setup runs no live query.

Native failure or interruption leaves command-pending because partial changes may exist. Inspect the client and protected backup. Acknowledge that inspection with `"action":{"type":"acknowledge-pending","confirmation_sha256":"<pending plan hash>"}`, then preview the current baseline. Setup does not automatically retry, overwrite a collision, force a change, or claim rollback.

Successful setup supplies a ready-to-send handoff for your chosen host agent. That agent handles model reasoning, `cqlmatic env inventory`, authorized bounded MCP searches and saved-result relay. You complete registration and browser consent.

The built-in inventory scout emits `repository:"search-all"` independently of the saved view. Relay it only with separate explicit authorization for that target/window. Otherwise leave scouting pending and use lower-level selected-source discovery on the confirmed view; later queries retain that view.


For all action inputs and phase transitions, use [the setup guide](../../docs/SETUP.md); for framing, use [contracts](../../docs/CONTRACTS.md). Requests use `action.type`, unlike inventory's `action.kind`. Setup normally exits 1 until a matching report includes exact available `falcon_search_ngsiem`, when it exits 0. Input/I/O/action errors exit 2. A valid preview/detect result is therefore usually exit 1, not a failed native operation.

No-request `setup --json` reads saved status without creating state or probing clients. An explicit `status` action request passes through the protected writer and can save/create state. A `cancel` action saves progress but cannot clear an unresolved pending native command. Keep the same explicit private `--state` path throughout one setup; use a different state for a different client/environment workflow.
