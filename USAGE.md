# CQLmatic command reference

This reference describes CQLmatic **0.1.3**. Start with the [documentation index](docs/README.md), [setup](docs/SETUP.md), and [inventory](docs/INVENTORY.md). Machine callers should also read the [contracts reference](docs/CONTRACTS.md); query authors should use the [query reference](docs/QUERY-REFERENCE.md).

Commands below use `cqlmatic` as shorthand for your executable path or the published `npx cqlmatic@0.1.3` (a [supported, patched Node.js release](README.md#try-v013), macOS Apple silicon or Linux x86_64). Run `npx` outside the CQLmatic source checkout. Windows x64 users can use the verified [release ZIP](docs/SETUP.md#install-on-windows). From a source build use `./target/release/cqlmatic`; on Windows it is `.\target\release\cqlmatic.exe`. See [development](docs/DEVELOPMENT.md) for the pinned Rust 1.99.1 build. Runtime use needs no Rust installation or model API key. CQLmatic makes no internal model calls. Relative file paths resolve from your current directory; save example inputs locally or extract the source release before using `examples/` paths.

## Help, version, and schemas

```sh
cqlmatic --help
cqlmatic --version
cqlmatic generate --help
cqlmatic env discover --help
cqlmatic --schema generate
```

`-h`/`--help` and `-V`/`--version` are parser-provided help/version options. Use command-specific `--help` for nested commands. A bare invocation is an input error (exit `2`), not an interactive default.

`--schema NAME` prints a bundled JSON Schema and exits `0`. Names are `generate`, `check`, `pack`, `discovery`, `result`, `assess`, `setup`, `inventory`, and `status`. It cannot accompany a command. Some exports contain `request`/`result`/`state` subschemas; do not send the whole schema wrapper as a request. See [schema selection](docs/CONTRACTS.md#schema-exports).

## Input and output rules

The native framing and exit rules below describe the executable itself. npm can add its own stderr notices, and a signal sent only to the npm parent PID may leave the command running. Use `npx` in a foreground terminal for interactive setup; use the native executable directly when a supervisor requires reliable process cancellation or a machine caller requires native stderr framing.

- JSON and CQL inputs are UTF-8. Supply exactly one input mode for `generate`, `check`, or `assess`.
- `-` means stdin for JSON request files, generation plan/params files, raw check input (`check -` or `--file -`), and assessment query/result files. Environment JSON options `--environment` and discovery `--request` also use this convention. State, pack, credentials, import source/output, and MCP answer paths are filesystem paths. Do not select stdin for two file arguments in one invocation.
- `generate`, `check`, and `assess` support `--jsonl` on stdin. It implies structured output; no `--json` is needed. Setup, inventory, and status take one request, not JSONL.
- `--json` is available only where listed below. Catalog, environment inspection/import/discovery/identity/parser commands already emit JSON without it.
- Generation/checking JSON is an `Analysis`; assessment JSON is an `AssessResult`. Setup, inventory/status, and discovery have different contracts. Human output is for people, not a stable parser interface.
- Application-level global failures emit a standalone JSON diagnostic on stderr and exit `2`, including with `--json`. Argument-parser failures can be plain text. Read stdout, stderr, and the exit code. A late output failure can occur after useful rows or file writes.

[Contracts and streaming](docs/CONTRACTS.md#streams-and-errors) explains framing, IDs, blank records, record errors, and partial output.

## Setup, status, and inventory

| Command | Options | Result and effects |
| --- | --- | --- |
| `setup` | `--request FILE`, `--state FILE`, `--json` | Terminal wizard without a request; strict setup action with one; no-request JSON/nonterminal invocation reads saved status. Native Codex, VS Code Local, Claude Code and Copilot adapters preview and apply reviewed configuration; other hosts use tracked external selection and saved report import. |
| `env inventory` | `--request FILE`, `--state FILE`, `--json` | Terminal guided inventory; actions save workflow evidence and may fetch parser metadata or prepare/resume MCP discovery. No-request JSON/nonterminal invocation reports status. |
| `status` | `--request FILE`, `--state FILE`, `--json` | Offline checklist. The request may select `state`; do not also select it with `--state`. |

Use a dedicated private state directory. Default OS-specific locations and every action are described in [setup](docs/SETUP.md) and [inventory](docs/INVENTORY.md). Setup actions use `action.type`; inventory actions use `action.kind`. Inventory has no caller `id` field.

```sh
cqlmatic setup --json
cqlmatic env inventory --json
cqlmatic status --json
```

These no-request invocations do not start native setup or a search. A setup request with action `status` passes through the state writer; it is different from no-request `setup --json`. Setup exits `1` until saved tool-discovery evidence qualifies. Inventory/status exit `0` when their command succeeds even if `full_map_complete` is false or an action records incomplete/failed work. Inspect the returned fields.

Native setup apply needs the exact reviewed confirmation hash. Interactive consent cannot run through unattended JSON apply: `SETUP_INTERACTIVE_REQUIRED` is a global error, exit `2`. Successful native configuration is not authentication or tool availability. Parser API reads use a separate protected credential file and explicit budgets; MCP search uses the selected host client's OAuth. The lower-level `--execute` search switch does not gate every network-capable setup/inventory action.

For native setup, select `claude-code` with allowlisted `cimd`, or `copilot-cli`, `copilot-app`, `copilot-vscode`, `codex`, or `vs-code` with a registered manual client ID. Preview and apply the reviewed host action, then import a bound report. Claude/Copilot reports must include the effective OAuth client ID and configuration source from the selected session. `external` alone uses `registration.mode: "host-managed"` and a matching `host_id`; native actions are unavailable for it. A bound authenticated report without exact available `falcon_search_ngsiem` remains incomplete with exit `1`. See [native host setup](docs/SETUP.md#native-hosts-and-effective-configuration).

Inventory's built-in scout always emits MCP `repository:"search-all"`, even when its saved environment view differs. Relay it only with explicit authorization for that target/window. Otherwise leave scouting pending and use confirmed selected sources with lower-level discovery on the authorized view. Scout import does not automatically reject an unflagged empty or cap-sized census; `scouting:done` is not a completeness attestation. See [scouting boundaries](docs/INVENTORY.md#action-inputs-and-evidence-boundaries).

## Generate

Choose exactly one input mode:

| Mode | Input | Scope |
| --- | --- | --- |
| `--request FILE` | Full v1 `GenerateRequest` | Request's input and context. |
| `--plan FILE` | A typed `Plan`, not a request wrapper | Required `source` object in the plan, including `{}` if unscoped. |
| `--recipe ID --params FILE` | Recipe ID and its parameter object | Recipe parameters; both flags are required together. |
| `--intent TEXT` | The bounded-English grammar | Optional `--repo NAME`, `--type NAME`, `--event NAME` supply literal selectors. |
| `--jsonl` | One full v1 request per stdin record | Each record's scope/context; one process-wide optional pack. |

The source flags belong only to `--intent`. A structured English input requires `source`, even when it is `{}`.

```sh
cqlmatic generate --request examples/generate.request.json --json
cqlmatic generate --plan examples/process-count.plan.json --json
cqlmatic generate --recipe threshold --params examples/threshold.params.json --json
cqlmatic generate --intent 'count events where user equals "synthetic-user"' --json
cqlmatic generate --jsonl < examples/requests.jsonl
```

### Shared generation and check flags

| Flag | Default | Behavior |
| --- | --- | --- |
| `--json` | Human output | Compact newline-terminated `Analysis`. |
| `--jsonl` | Off | Read versioned requests from stdin; continue after record errors. |
| `--pack FILE` | None | Read and validate one local pack; never refresh it or select the inventory's active pack implicitly. |
| `--inventory-state FILE` | Local inventory state | Use a protected inventory's confirmed target and query-observation ledger. In Quick mode, accepted query source/input-field names can be saved as unverified leads. |
| `--require-observed` | False | Require qualifying current pack or saved-query field evidence for referenced deployment fields; without either, strict grounding needs context. |
| `--accept-partial-coverage` | False | In strict mode, accept current positive partial source/field observations with a warning. Stale, absent and ambiguous evidence still blocks. |
| `--fail-on-warning` | False | Return `1` on warning diagnostics even when `outcome` remains `ready`; informational diagnostics do not trigger it. |
| `--accept-mixed-record-kinds` | False | Permit kind ambiguity with a warning; use only after the user explicitly accepts mixing kinds. |

The three policy flags `--require-observed`, `--accept-partial-coverage` and `--accept-mixed-record-kinds` are ORed with the corresponding request context values. Omitting a flag cannot disable a request's true value. No CLI flag supplies `confirmed_fields` or `context.environment`; use a full request for those fields. A confirmed Quick target can supply missing environment context, but never replace an explicit request or pack identity; the result says when it did so. Saved-query evidence depends on the caller's assertion that the query, response, target and window belong together.

Human generation prints the query only when ready; on other outcomes stdout can be empty. Diagnostics and grounding problems go to stderr. JSON includes the interpretation and evidence; generation withholds `query` on every nonready outcome. Inspect the returned plan before executing the exact query through an authorized client.

## Bounded English

English is a deterministic adapter, not unrestricted intent interpretation. It does not infer fields, exclusions, source mapping, or time windows. See the [full grammar and examples](docs/QUERY-REFERENCE.md#bounded-english). For requests outside the grammar, use a recipe or typed plan. Matching, missing-field policy, exactness, and Boolean grouping remain explicit.

## Check and lint CQL

Choose exactly one of positional `QUERY`, `--file FILE`, `--request FILE`, or `--jsonl`. Shared flags above apply.

```sh
cqlmatic check 'count()' --json
cqlmatic check --file .local/query.cql --pack .local/pack.json --json
cqlmatic check --request .local/check.request.json --json
cqlmatic check --jsonl < examples/checks.jsonl
```

Checking never executes or returns a copy of the input: `Analysis.query` is `null`, even for `ready`. Keep the exact input separately. Human check prints an assessment line on stdout plus diagnostics on stderr.

Empty/whitespace input and `*` are valid match-all checks. Static acceptance does not ensure restrictive scope. `unsupported` can mean valid platform syntax outside the local profile or a typo; `invalid` is a recognized checked-rule violation. See [coverage](docs/COVERAGE.md) and [raw CQL checking](docs/QUERY-REFERENCE.md#raw-cql-checking).

`groupBy(..., limit=max)` emits informational `CQL_GROUPBY_LIMIT`; numeric or omitted grouping limits warn. Therefore `--fail-on-warning` does not reject the informational form. Group and memory ceilings still apply.

## Assess saved results

Choose exactly one of `--request FILE`, `--jsonl`, or the pair `--query-file FILE --result FILE`. Optional flags are `--pack FILE` and `--json`; generation/check policy flags do not apply.

```sh
cqlmatic assess --request examples/assess.request.json --json
cqlmatic assess --query-file .local/query.cql --result .local/mcp-result.json --pack .local/pack.json --json
```

Save the entire unedited MCP result and the exact executed query bytes. The command is offline. Human output prints findings and a summary on stdout; structured output contains counts, column statistics, caps, and findings. Outcomes are `no-findings` (`0`), `advisory` or `inconclusive` (`1`), and `error` (`2`). `no-findings` does not prove correctness or completeness. See [result assessment](docs/RESULT-ASSESSMENT.md).

## Look up syntax and fields

```sh
cqlmatic catalog cql:workflow:analyst --full --limit 1
cqlmatic catalog join --full --limit 3
cqlmatic catalog --functions regex
cqlmatic catalog --functions --limit 100
cqlmatic catalog --recipes
cqlmatic catalog --profile
cqlmatic catalog --standard cps event
cqlmatic catalog --standard ecs process.name
```

| Argument/flag | Default | Meaning |
| --- | --- | --- |
| `TERM` | Empty | Case-insensitive search. Ordinary lookup searches IDs, titles, bodies; an exact ID takes priority. Function lookup uses a function-name substring. |
| `--functions` | Off | Executable function metadata; includes required/positional/named arguments, type codes, effects, placement, and scalar-computation flag. |
| `--recipes` | Off | All nine recipe IDs and descriptions. |
| `--profile` | Off | Profile ID, knowledge revision, assurance statement, inventory counts, parser budgets, span convention. |
| `--standard cps\|ecs` | None | Standard field-name leads, not tenant observations. |
| `--full` | Off | Full ordinary knowledge records, including bodies and sources; does not expand function metadata into prose. |
| `--limit N` | 10 | 1–100 matches for ordinary/functions/standard lookup. Recipe/profile modes emit their complete object/list. |

At most one of `--functions`, `--recipes`, `--profile`, and `--standard` may be selected. All modes are offline JSON output. Compact ordinary records include `supported`, which describes the associated local reference coverage; it is not execution proof. Broader knowledge can describe syntax outside the checker. See the [function/catalog reference](docs/QUERY-REFERENCE.md#function-contracts).

## Environment commands

### Identity

```sh
cqlmatic env identity --cloud us1 --view search-all --credentials-file PATH_TO_PROTECTED_CREDENTIALS
```

`--cloud us1|us2|eu1` and `--view NAME` are required. `--credentials-file FILE` is optional; without it, the explicit Falcon environment variables are used. The command reads credentials but makes no network request. It prints an `EnvironmentId` with an API-client fingerprint, not a provider-verified CID. Keep the output protected. [Identity and credential rules](docs/ENVIRONMENT.md#bind-an-account) explain binding and rotation.

### Inspect

```sh
cqlmatic env inspect .local/pack.json
cqlmatic env inspect .local/pack.json --source SOURCE_ID --field user --limit 20
cqlmatic env inspect .local/pack.json --full
```

`FILE` is required. `--source TEXT` matches an exact source ID, display name, or repository selector; it does not directly match a type selector. Prefer a source ID. `--field TEXT` is a case-sensitive path substring. `--limit N` defaults to `20` (range 1–100), independently bounding field and value records in compact filtered output.

Without filters the default view includes all source records, field/value counts, annotations, identity, digest, and date. `--summary` returns a smaller source-selector/count overview; it cannot be combined with `--full`, `--source`, or `--field`. With filters the detailed view returns `environment`, `pack_id`, selected fields and their sampled values. `--full` returns the pack-shaped view and ignores `--limit`; optional filters still apply, retaining only related records and provenance. A filtered full view retains the original digest and is **not** an importable replacement snapshot. Use the original unfiltered file for import. Inspection is offline and never reads credentials.

### Import

```sh
cqlmatic env import --from snapshot.json --out .local/imported.json
cqlmatic env import --from examples/legacy-observation --environment examples/environment.json --out .local/example-pack.json
```

Required flags are `--from FILE_OR_DIRECTORY` and `--out FILE`. Native files must pass identity/integrity/profile/record validation. `--environment FILE` optionally asserts matching identity for native input and is required for legacy directory input. Choose a **separate output path** that does not alias the native input, environment file, or any legacy corpus component. Import reads inputs before writing the chosen destination but does not enforce that separation; `--replace` can overwrite an input selected as output, including a legacy `sources.jsonl`. Inputs stay unchanged only when output is separate. Replacement does not perform a refresh merge. The JSON report identifies the imported path/digest. A durability failure can report `active_pack_updated:true` with exit `2`; inspect the output before retrying. See [import mechanics](docs/ENVIRONMENT.md#import-and-inspect).

### Discover

| Mode | Required options | Additional options / constraints |
| --- | --- | --- |
| Plan only | `--request FILE` | No credentials or search; prints initial probes, deferred stages, budgets. This is the default. Pack/receipt/credential flags do not make it execute. |
| Native API execution | `--request FILE --execute --pack FILE --receipt NEW_FILE` | Optional `--credentials-file FILE`; otherwise Falcon environment variables. New receipt path, exact environment binding, explicit finite budgets. |
| MCP initialization | `--mcp --request FILE --state NEW_FILE --pack FILE --receipt NEW_FILE` | No `--execute`, credentials flag, or answers. Creates local relay state and emits probes. |
| MCP resume | `--mcp --state FILE` | Repeatable `--answer PROBE_ID=FILE`; omit answers for an activation/status retry. No request, pack, or receipt options. |

`--state` and `--answer` require `--mcp`. Answer files are entire saved MCP tool results, not edited rows. `needs-probes` and `activation-blocked` exit `3`; neither means complete. API execution and MCP relay have different lock, receipt, cleanup and recovery semantics. See [environment discovery](docs/ENVIRONMENT.md), [MCP workflow](docs/MCP-COMPANION.md), and [failure recovery](docs/TROUBLESHOOTING.md).

```sh
cqlmatic env discover --request examples/discover.request.json
```

The example above is a safe offline plan. Live searches require the operator's own explicit target, UTC bounds, authorization, permissions, and budgets. A pack's searchable `view` differs from a `#repo` selector.

### Parsers

`env parsers --environment FILE` accepts optional `--credentials-file FILE`, `--id ID`, `--template`, `--offset N`, and `--limit N`. It performs authenticated read-only metadata access with a 30-second deadline and returns backend JSON, not an automatically bound parser contract.

Without `--id`, offset defaults to `0` (maximum `9999`) and limit to `50` (range 1–100); pagination is explicit. With `--id`, retrieve that parser definition; `--template` retrieves its template and requires `--id`. IDs are bounded to 256 characters without controls. `--offset`/`--limit` are list options and do not page an ID lookup. Full inventory/review uses the [inventory workflow](docs/INVENTORY.md).

## Exit codes

| Command | `0` | `1` | `2` | `3` |
| --- | --- | --- | --- | --- |
| `generate`, `check` | Ready, no warning failure | Nonready assessment or `--fail-on-warning` | Error | — |
| `assess` | No findings | Advisory/inconclusive | Error | — |
| `setup` | Tool discovery verified | Setup remains pending | Input/native/storage/global error | — |
| `status`, `env inventory` | Command succeeded, possibly incomplete | — | Global error | — |
| `env discover` API | Complete and pack activated | — | Failure, including late activation/report failures | — |
| `env discover --mcp` | Complete activation | — | Failed/error | Needs probes or activation blocked |
| Other commands | Successful operation | — | Application error | — |

Argument-parser errors also return `2`; help/version return `0`. JSONL returns the highest record exit, unless a global failure ends the stream. Always inspect returned evidence and diagnostics: an exit alone does not establish inventory completion, unchanged files, or correct/complete search results.

## Environment and native configuration

| Variables | Where they apply |
| --- | --- |
| `FALCON_CLIENT_ID`, `FALCON_CLIENT_SECRET`, optional `FALCON_BASE_URL`, `FALCON_MEMBER_CID` | Native API commands/identity when no credential file is supplied. Not MCP OAuth or a model key. See [credential restrictions](docs/ENVIRONMENT.md#credentials). |
| `LOCALAPPDATA`, `XDG_STATE_HOME`, `XDG_DATA_HOME`, `HOME` | OS-local setup/inventory state selection; exact precedence and platform locations are in [setup storage](docs/SETUP.md#local-state-and-recovery). `--state` selects explicit workflow storage. |
| `CODEX_HOME`, `HOME`/Windows `USERPROFILE` | Setup's Codex native configuration target is `CODEX_HOME/config.toml`, otherwise the home directory's `.codex/config.toml`. This adapter targets that user configuration; it does not choose a project-local Codex configuration. Roots must be absolute and pass protected-path checks. |
| `APPDATA`, `XDG_CONFIG_HOME`, `HOME`/`USERPROFILE` | Default VS Code **Local harness** `Code/User/mcp.json` parent: Windows `APPDATA`, macOS home `Library/Application Support`, Linux `XDG_CONFIG_HOME` or home `.config`. Custom/remote profiles need native review. |
| `COPILOT_HOME`, `CLAUDE_CONFIG_DIR` | Native host configuration locations. Copilot CLI/app/VS Code Copilot use `$COPILOT_HOME/mcp-config.json` or `~/.copilot/mcp-config.json`; Claude Code uses `$CLAUDE_CONFIG_DIR/.claude.json` or `~/.claude.json`. Setup previews and changes the selected local entry; verify the effective entry in the intended host session. |
| `VSCODE_PORTABLE` | When present, the adapter refuses the portable-profile assumption (`SETUP_PROFILE`); use native manual review. |
| `PATH`, Windows `SystemRoot` | Installed-client/native executable discovery. Setup binds the canonical executable and capability hashes; requests cannot supply an arbitrary executable. |
| `TERM`, `NO_COLOR` | Interactive setup's ASCII banner may use color on recognized non-Windows terminals. `NO_COLOR` disables it; Windows/unrecognized terminals use plain text. JSON/scripted output has no banner. |
| `CQLMATIC_CARGO_HOME`, `CARGO_TARGET_DIR` | Build/development cache and artifact locations; see [development](docs/DEVELOPMENT.md). They do not choose an environment pack. |

Client configuration is version-sensitive. Native setup checks installed capabilities for command adapters and previews the exact command or Copilot file write. `external` records a guided selection and imports caller-supplied discovery evidence. Use [Codex MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli), [Claude Code MCP documentation](https://code.claude.com/docs/en/mcp), [Copilot CLI MCP reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference), and [VS Code MCP configuration](https://code.visualstudio.com/docs/agent-customization/mcp-servers) for host options. Session overrides and OAuth callback behavior still require host review.

## Use it from an agent

Follow the [analyst workflow](docs/ANALYST-WORKFLOW.md) and [MCP companion guide](docs/MCP-COMPANION.md). Use full requests for explicit context, retain query bytes, consume the documented output type for each command, select the pack explicitly, and inspect results after authorized execution. [Packaged agent instructions](integrations/agent/AGENTS-snippet.md) are optional guidance; no skill installation is required for the CLI.
