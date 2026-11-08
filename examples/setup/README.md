# Native-client setup examples

Start interactively with `cqlmatic setup`. `--json` without a request reports saved progress and never waits for input. A fresh status has no environment or credentials.

The JSON files here are synthetic contract examples, not evidence of a configured or authenticated client. Use a new dedicated private directory for `--state`; omitting it uses OS-local CQLmatic setup storage.

```text
cqlmatic setup --request examples/setup/detect.request.json --json
cqlmatic setup --request examples/setup/select.request.json --json
cqlmatic setup --request examples/setup/preview.request.json --json
```

`select.request.json` illustrates an explicit allowlist confirmation. Set it only after confirming the exact published metadata identity is currently supported by CrowdStrike and that your Falcon user has the required MCP role. Local detection cannot establish tenant access. VS Code CIMD requires stable 1.106.0 or later and the local `--add-mcp` capability. See [VS Code 1.106 release notes](https://code.visualstudio.com/updates/v1_106), reviewed 2026-11-06.

For Codex, choose `"client": "codex"` and the exact metadata document URL your current native client uses for the selected cloud. Its identity can depend on version and server capabilities; no universal ID is assumed. Ask your host agent to inspect the current official Codex MCP documentation and Falcon CIMD allowlist for that exact identity. If it cannot establish one, select manual registration. Setup never silently chooses DCR.

A preview names the absolute executable, exact argv array, native config target, original-config fingerprint, protected backup path, and setup-state changes. Review those details. To apply, create a request with `"action": {"type":"apply","confirmation_sha256":"<the exact reviewed 64-character hash>"}`. Do not reuse a hash after any selection, binary, capability, or configuration change. Codex add can immediately start native browser consent, so applying its command requires an interactive terminal. Applying a plan with an interactive command in JSON mode or without a TTY returns global error `SETUP_INTERACTIVE_REQUIRED` and exit 2 before running the command. It does not emit a successful pending setup result.

Manual registration uses `"registration":{"mode":"manual","client_id":"synthetic-generated-id","callback_url":"http://127.0.0.1:1234/callback"}`. Replace both values with the admin-generated MCP client ID and exact callback URI reported by the native client. Optional secrets go only through the client's protected native UI. Never put secrets in a request, CLI argument, setup state, or source control.

Advanced service mode uses `"registration":{"mode":"api-client","credentials_file":"<absolute protected file path>"}`. Setup records that reference without opening it and remains advanced-pending until a supported native client/plugin is configured. CQLmatic does not perform OAuth or MCP transport.

After native browser sign-in, `"action":{"type":"confirm-authentication"}` records user confirmation only. Configuration, native exit status, and MCP list output do not attest authentication. Ask the chosen host agent to perform native tool discovery and save an actual report in the strict shape shown by `tool-discovery.json`; never import this synthetic file as real evidence. Import with `"action":{"type":"import-discovery","file":"<saved report path>"}`. The selected client, server name and canonical cloud endpoint must match. Only exact available `falcon_search_ngsiem` reaches tool-discovery-verified; a bound authenticated report without that tool remains authenticated with exit 1. Reports are caller-supplied saved evidence, not independent transport attestation, and setup runs no live query.

Native failure or interruption leaves command-pending because partial changes may exist. Inspect the client and protected backup. Acknowledge that inspection with `"action":{"type":"acknowledge-pending","confirmation_sha256":"<pending plan hash>"}`, then preview the current baseline. Setup does not automatically retry, overwrite a collision, force a change, or claim rollback.

Successful setup supplies a ready-to-send handoff for your chosen host agent. That agent handles model reasoning, `cqlmatic env inventory`, authorized bounded MCP searches and saved-result relay. You complete registration and browser consent.


For all action inputs and phase transitions, use [the setup guide](../../docs/SETUP.md); for framing, use [contracts](../../docs/CONTRACTS.md). Requests use `action.type`, unlike inventory's `action.kind`. Setup normally exits 1 until a matching report includes exact available `falcon_search_ngsiem`, when it exits 0. Input/I/O/action errors exit 2. A valid preview/detect result is therefore usually exit 1, not a failed native operation.

No-request `setup --json` reads saved status without creating state or probing clients. An explicit `status` action request passes through the protected writer and can save/create state. A `cancel` action saves progress but cannot clear an unresolved pending native command. Keep the same explicit private `--state` path throughout one setup; use a different state for a different client/environment workflow.
