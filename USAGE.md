# CQLmatic command reference

This reference describes CQLmatic **0.1.13** and its executable profile, `cqlmatic-ngsiem-core-v3`. Earlier packages retain their shipped behavior; inspect `cqlmatic catalog --profile` and command-specific help for the binary you run. Start with the [documentation index](docs/README.md), [setup](docs/SETUP.md), and [inventory](docs/INVENTORY.md). Machine callers should also read the [contracts reference](docs/CONTRACTS.md); query authors should use the [query reference](docs/QUERY-REFERENCE.md).

See [hosted search outcomes](docs/MCP-COMPANION.md#how-to-tell-whether-a-hosted-search-finished) for completion and error shapes, retry guidance and the positive control required for zero results. For refresh and capability settlement, use `not-started` for a recognized start rejection and `timed-out` for a hosted cutoff. Keep lost responses, client waits without a tool answer, transport errors and unrecognized errors `unresolved`. A recognized poll failure from the hosted search tool shows the search had started by the time the answer arrived. Use that arrival time for `search_started_at` and submit the exact bound `lifetime-expired` settlement with `recorded_at` at least the retained provider lifetime later. Record the saved answer's SHA-256 and trace ID in `evidence`. For capability, also supply the saved answer as `result_file`. The refresh imports no rows and the capability case stays `unknown`.

Commands below use `cqlmatic` as shorthand for your executable path or `npx cqlmatic@0.1.13` (a [supported, patched Node.js release](README.md#try-v0113), macOS Apple silicon or Linux x86_64). Run `npx` outside the CQLmatic source checkout. Windows x64 users can use the [release ZIP](docs/SETUP.md#install-on-windows) after verifying its checksum. From a source build use `./target/release/cqlmatic`; on Windows it is `.\target\release\cqlmatic.exe`. See [development](docs/DEVELOPMENT.md) for the pinned Rust 1.99.1 build. Runtime use needs no Rust installation or model API key. CQLmatic makes no internal model calls. Relative file paths resolve from your current directory; save example inputs locally or extract the source release before using `examples/` paths.

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
| `env inventory` | `--state FILE`, `--json`, `--brief` or `--summary`, `--request FILE` or `--next-probe [--run-index N] [--probe-id ID --result-file FILE]` | Terminal guided inventory. With `--json`, `--next-probe` settles an optional answer and returns the outstanding or next ready probe. Other actions save workflow evidence and can fetch parser metadata or prepare/resume discovery. Without an action, JSON or nonterminal invocation reports status. |
| `env capture` | `--client CLIENT`, `--server NAME`, `--state FILE`, `--install` with optional `--dry-run`, `--program TEXT` or `--hooks LIST` | The hook reads a finished search from stdin, records it in the confirmed inventory, and exits 0 silently. For `claude-code` and `codex`, a search reported before it runs gets check's verdict instead: one line of JSON that adds a note, or nothing, with exit status 0. It never refuses the search. With `--install`, the command registers the hooks where supported and reports each hook's result and whether it needs manual setup; see [checking each search before it runs](docs/SETUP.md#check-each-search-before-it-runs). `--dry-run` writes nothing and sets `changed` if installation would edit the file. See [Scrappy mode](docs/INVENTORY.md#scrappy-mode-capture-the-agents-searches). |
| `env pass-through` | `--state FILE`, then `-- COMMAND [ARGUMENTS]` | Runs an MCP server command, passes its messages through unchanged and records finished searches like `env capture`, for clients without a usable hook. Standard output carries only the server's messages. |
| `status` | `--request FILE`, `--state FILE`, `--json`, `--brief` or `--summary` | Offline checklist. The request may select `state`; do not also select it with `--state`. |

Use a dedicated private state directory. Default OS-specific locations and every action are described in [setup](docs/SETUP.md) and [inventory](docs/INVENTORY.md). Setup actions use `action.type`; inventory actions use `action.kind`. Inventory has no caller `id` field.

```sh
cqlmatic setup --json
cqlmatic env inventory --json
cqlmatic status --json
```

For agent communication, use `cqlmatic status --json --brief` and `env inventory --request FILE --json --brief`. Brief projections are at most 4,096 bytes and omit operational payloads. The `next-probe` action returns one exact execution descriptor when a probe is available, even with `--brief` and for arguments above that limit. Brief mode conflicts with `--summary`. See [bounded handoff](docs/INVENTORY.md#bounded-agent-handoff) for aggregate notices, acknowledgement and protected handling of execution descriptors.

These no-request invocations do not start native setup or a search. A setup request with action `status` passes through the state writer; it is different from no-request `setup --json`. Setup exits `1` until saved tool-discovery evidence qualifies. Inventory/status exit `0` when their command succeeds even if `full_map_complete` is false or an action records incomplete/failed work. Inspect the returned fields.

Native setup apply needs the exact reviewed confirmation hash. Interactive consent cannot run through unattended JSON apply: `SETUP_INTERACTIVE_REQUIRED` is a global error, exit `2`. Successful native configuration is not authentication or tool availability. Parser API reads use a separate protected credential file and explicit budgets; MCP search uses the selected host client's OAuth. The lower-level `--execute` search switch does not gate every network-capable setup/inventory action.

For native setup, select `claude-code` with allowlisted `cimd`, or `copilot-cli`, `copilot-app`, `copilot-vscode`, `codex`, or `vs-code` with a registered manual client ID. Preview and apply the reviewed host action, then import a bound report. Claude/Copilot reports must include the effective OAuth client ID and configuration source from the selected session. `external` alone uses `registration.mode: "host-managed"` and a matching `host_id`; native actions are unavailable for it. A bound authenticated report without exact available `falcon_search_ngsiem` remains incomplete with exit `1`. See [native host setup](docs/SETUP.md#native-hosts-and-effective-configuration).

Full-mode inventory scouting emits MCP `repository:"search-all"`, even when its saved environment view differs. Quick scouting uses the explicitly chosen view. Relay it only with explicit authorization for that target/window. Otherwise leave scouting pending and use confirmed selected sources with lower-level discovery on the authorized view. Scout import does not automatically reject an unflagged empty or cap-sized census; `scouting:done` is not a completeness attestation. See [scouting boundaries](docs/INVENTORY.md#action-inputs-and-evidence-boundaries).

At the start of an authorized environment session, inspect `status --json --brief` once for pending attempts, `refresh_reminder` and work notices. Automatic startup refresh is unavailable. Manual **Refresh** in `cqlmatic env inventory` and Settings prepares an action without executing MCP calls. A due 30-day reminder offers **Run** or **Remind me in 30 days**; agents present its hint before acknowledging the exact cycle, and status reads never acknowledge it. Manual refresh requires protected proof of a provider-enforced lifetime of 1–600 seconds for the exact session, target and authorized window. An advertised default or client wait timeout is insufficient. Reserve, save, settle and import the exact single call; unresolved attempts block another refresh. The 24h/7d/14d/30d census lookback is separate from the fixed reminder interval. New work can be reviewed now or deferred with **Settings \*** retained. See [session entry](docs/MCP-COMPANION.md#when-an-environment-session-begins) for authorization, exact-target and separate reminder/work-notice handling.

### Optional function capability checks

Once a field pack is active, run `cqlmatic env inventory` and select **capabilities (optional)** to review the fixed checks. They are off by default. Empty input or **later** returns without preparing a run, and the activated field pack stays available. `status --json` reports retained case outcomes separately from field discovery; status reads do not execute a check.

The case IDs are `array-union`, `array-intersection`, `array-reduce-row`, `array-reduce-column` and `math-sqrt`. Interactive preparation defaults to the four array cases only after choosing **prepare**; scripted preparation supplies a nonempty, distinct `case_ids` list. Review the displayed fixed queries and authorize a separate finite attempt/time allowance before relaying searches.

Scripted actions use the normal inventory request and state selection:

```sh
cqlmatic env inventory --state .local/inventory.json --request .local/capability-action.json --json
cqlmatic status --state .local/inventory.json --json
```

| `action.kind` | Required action fields | Effect |
| --- | --- | --- |
| `prepare-capabilities` | `execution_context_id`, `session_id`, `case_ids`, `host_evidence_file`, `max_queries`, `max_seconds` | Retains the reviewed case revisions, target and original allowance; requires an activated field pack and protected provider-enforced search-lifetime evidence. |
| `dispatch-capability` | `run_id`, `case_id`, `execution_context_id`, `session_id` | Reserves one exact attempt and returns its MCP arguments. It does not execute the search. |
| `submit-capability` | `run_id`, `attempt_id`, `execution_context_id`, `settlement_file`, `retrieved_at`; optional `result_file` | Imports the full protected capture and exact-attempt terminal settlement. A completed attempt requires its matching result. |
| `pause-capabilities` | `run_id` | Pauses safely settled work; unresolved termination keeps its active budget running. |
| `resume-capabilities` | `run_id`, `execution_context_id` | Resumes the retained run with its original remaining allowance. |

The host evidence uses the confirmed environment/session and a provider-enforced finite search lifetime; a client wait timeout is insufficient. Execute each reservation once through that authorized host, save the entire response and exact settlement, then submit them. Failures and retries consume attempts. A lost response or unresolved termination blocks subsequent inventory dispatch until the same attempt is reconciled; no cancellation is inferred.

`max_seconds` must exceed the host evidence's `max_search_seconds`, because each dispatch needs that much active time left, counted from preparation. A paused run needs more than `max_search_seconds` left to resume, because its next dispatch comes after resuming. While a retained run in the same host session can still dispatch, preparing a case it holds at its current revision is refused under any execution context, so its allowance is not renewed; use that run with its own `run_id` and `execution_context_id`, resuming it if it is paused. A run whose cases at their current revision are all `verified` dispatches no further attempts; prepare a new run to check a case again. While a run for the same execution context and session can still dispatch and is active, pause it before preparing other cases or resuming another run. A run that can no longer dispatch, such as one with too little active time left, does not block a new preparation and keeps its history. An inventory retains at most 100 capability runs; use a separately initialized inventory for further checks. When a case cannot be reserved, the refusal says which condition stopped it and what to do next, including another retained run that already holds the case. Settle an unresolved attempt with `submit-capability`.

Evidence qualifies only the current case revision, argument shape, resource binding, exact environment/CID/view and caller-attested authorization context. The current reduction cases cover `sum(var)` forms, and the current fixed cases use no external resource binding. A different aggregate, omitted/extra argument, target or context does not inherit the observation. Positive results expire 30 days after retrieval; a newer unknown result within that same scope supersedes an older positive. Another authorization context neither inherits nor revokes it. Timeouts, errors and unexpected output stay unknown. No-op success or a beta marker does not verify capability behavior.

Use the matching `--execution-context-id` on generation/checking, or `context.execution_context_id` in a full request, to qualify that evidence. It does not replace field grounding or make every function form available. See [scope and limits](docs/COVERAGE.md#optional-target-capability-checks).

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
| `--pack FILE` | Matching inventory's active pack, when available | Select and validate a primary snapshot explicitly; a matching inventory's active pack and query ledger still supply safeguards. Never triggers discovery. |
| `--inventory-state FILE` | Local inventory state | Reuse a protected inventory's confirmed target, active pack and query-observation ledger in Full or Quick mode. Both modes can queue bounded unverified source/input-field hints; existing Quick leads are retained. |
| `--execution-context-id ID` | None | Caller-attested current authorization context for exact capability observations; never inferred from CID. When supplied, overrides the request's `context.execution_context_id`. |
| `--require-observed` | False | Require qualifying current pack or saved-query field evidence for referenced deployment fields; without either, strict grounding needs context. |
| `--accept-partial-coverage` | False | In strict mode, accept current positive partial source/field observations with a warning. Stale, absent and ambiguous evidence still blocks. |
| `--fail-on-warning` | False | Return `1` on warning diagnostics even when `outcome` remains `ready`; informational diagnostics do not trigger it. |
| `--accept-mixed-record-kinds` | False | In strict observed mode, accept mixed kinds with a warning after the user permits it. Ordinary mode already reports kind ambiguity as information. |

The three policy flags `--require-observed`, `--accept-partial-coverage` and `--accept-mixed-record-kinds` are ORed with the corresponding request context values. Omitting a flag cannot disable a request's true value. No CLI flag supplies `confirmed_fields` or `context.environment`; use a full request for explicit values. A confirmed Full or Quick inventory can fill otherwise unspecified environment context, reported as `CQL_QUICK_TARGET`; it never replaces an explicit request or pack identity. Inventory selection is checked separately for each JSONL row. Saved-query evidence depends on the caller's assertion that the query, response, target and window belong together.

A missing default state creates nothing and leaves basic commands available. An explicitly selected missing state, or a matching active snapshot that is missing, corrupt or unsafe, blocks the result even without `--require-observed` and even with an older explicit pack. An invalid default inventory can be ignored with `CQL_QUICK_INVENTORY_IGNORED` when a valid explicit pack is supplied; unsafe protected paths still fail closed. Foreign inventory produces `CQL_QUICK_TARGET_MISMATCH` without opening its active snapshot or capturing leads. Retained snapshots without an active selection require repair rather than an inferred choice.

Human generation prints the query only when ready; on other outcomes stdout can be empty. Diagnostics and grounding problems go to stderr. JSON includes the interpretation and evidence; generation withholds `query` on every nonready outcome. Inspect the returned plan before executing the exact query through an authorized client.

## Bounded English

English is a deterministic adapter, not unrestricted intent interpretation. It does not infer fields, exclusions, source mapping, or time windows. See the [full grammar and examples](docs/QUERY-REFERENCE.md#bounded-english). For requests outside the grammar, use a recipe or typed plan. Matching, missing-field policy, exactness, and Boolean grouping remain explicit.

Typed input also has bounded forms: it does not bind unresolved dashboard/user parameters, saved-query helpers or parser macros. Those remain unsupported rather than being substituted from ambient state.

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

`CQL_GROUPBY_LIMIT` is informational for every grouping limit, so `--fail-on-warning` does not reject it. With `limit=max` it says group and memory ceilings still apply; with a numeric or omitted limit it says groups can be discarded before a later sort or row cap.

With an environment map, from `--pack` or a matching inventory, generation and checking warn about what the map lacks. `CQL_UNMAPPED_SOURCE` reports a `#repo` or `#type` value that matches no mapped source, names mapped selectors, and says so when that value is the target view's name; a `#repo` equal to the view's name is reported even when a mapped source has no recorded repository. `CQL_UNMAPPED_FIELD` compares each field the query reads with the selected sources; a query without `#repo` or `#type` is compared with every mapped source. Each selected source is judged by its own records, and the field warns only when no selected source has it. The source's record for that exact path decides: observed, documented or declared is quiet, and measured, not observed and not declared warns with its measurement window. With no exact record, a member of an object or array the source documents is quiet, and any other field warns. Paths compare segment by segment, ignoring the number in an array index but not its position: `event.category[9]` matches `event.category[0]`, `event[0].category` does not, and a read of a whole array such as `x[]` matches the records of its elements. `#name` and `name` are different fields. A source's recorded datasets and record-kind fields count as listed. A field the query wrote earlier is not reported, nor is an array that a function reads directly after the query wrote one of its elements, and neither are `@` metadata fields. A function that reads such an array through `var`, such as `array:exists` or `array:filter`, can still report it. `in()` on `#repo` or `#type` is not compared: a query that uses it gets no `CQL_UNMAPPED_FIELD`, and a literal `#repo` or `#type` beside it is still checked for `CQL_UNMAPPED_SOURCE`. A selector with a wildcard, a regex or several values is not compared, but a literal selector beside it is. Without a map these warnings do not apply; `grounding.basis` keeps the evidence for each field either way. `--require-observed` keeps the source warning and reports ungrounded fields through strict grounding.

Reading a field that an earlier step removed, such as `drop(src.ip) | src.ip=*`, warns `CQL_DROPPED_FIELD`, with or without a map, but only when no step between them can create fields: a parser, an assignment or any other function that writes fields keeps it quiet.

Performance notes are informational too. `ComputerName=a | #event_simpleName=ProcessRollup2` reports `CQL_TAG_FILTER_ORDER`, and `y := lower(a) | ComputerName=b` reports `CQL_FILTER_AFTER_TRANSFORM`: filter on `#` fields first and transform last. `CQL_SORT_COST`, `CQL_SPLIT_COST`, `CQL_WILDCARD_IGNORE_CASE` and `CQL_REGEX_ALL_FIELDS` cover single stages; see [coverage](docs/COVERAGE.md).

## Assess saved results

Choose exactly one of `--request FILE`, `--jsonl`, or the pair `--query-file FILE --result FILE`. Optional flags are `--pack FILE`, `--inventory-state FILE` and `--json`; generation/check policy flags do not apply. Assessment reuses a matching inventory's active pack unless an explicit pack is selected, and never saves leads or converts the query ledger into population rates.

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
cqlmatic catalog --functions --limit 250
cqlmatic catalog --recipes
cqlmatic catalog --profile
cqlmatic catalog --standard cps event
cqlmatic catalog --standard ecs process.name
```

| Argument/flag | Default | Meaning |
| --- | --- | --- |
| `TERM` | Empty | Case-insensitive search. Ordinary lookup searches IDs, titles, bodies; an exact ID takes priority. Function lookup uses a function-name substring. |
| `--functions` | Off | Bounded executable contracts; includes required/positional/named arguments, type codes, effects, placement, scalar-computation flag and target prerequisites. |
| `--recipes` | Off | All nine recipe IDs and descriptions. |
| `--profile` | Off | Profile ID, knowledge revision, assurance statement, inventory counts, parser budgets, span convention. |
| `--standard cps\|ecs` | None | Standard field-name leads, not tenant observations. |
| `--full` | Off | Full ordinary knowledge records, including bodies and sources; does not expand function metadata into prose. |
| `--limit N` | 10 | 1–250 matches for ordinary/functions/standard lookup. Recipe/profile modes emit their complete object/list. |

At most one of `--functions`, `--recipes`, `--profile`, and `--standard` may be selected. All modes are offline JSON output. An exact function name ranks its record first in ordinary and `--functions` searches. Compact ordinary records include `supported`, which describes the associated local reference coverage; it is not execution proof. Broader knowledge can describe syntax outside the checker. See the [function/catalog reference](docs/QUERY-REFERENCE.md#function-contracts).

The current source profile is `cqlmatic-ngsiem-core-v3`: 194 Query candidate names, comprising 193 current candidates and deprecated `lowercase`. The [official-page ledger](data/function-scope.json) separately excludes parser-only `copyEvent` and unreleased `math:spherical2cartesian`. This is name membership with bounded contracts, not confirmation of every documented form or full vendor-compiler coverage. `catalog --profile` reports the actual binary's profile and knowledge revision; pack compatibility uses their explicit metadata binding.

Raw checking accepts the conservative regex subset with distinct `i/m/d/F` flags; `F` requests the vendor engine without proving its runtime behavior. `g` remains unsupported; qualified repeated capture uses `regex(repeat=true)`. Saved calls and unresolved parameters have no substitution interface. Lookup, typed lowering, raw parsing, static acceptance, grounding and saved-result assessment remain distinct from live execution.

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

Without filters the default view includes all source records, field/value counts, annotations, identity, digest, and date. `--summary` returns a smaller source-selector/count overview. Both views include `inventory_source_id` for sources that map directly to an inventory selector. Use that ID in inventory actions and the original `id` for pack inspection. `--summary` cannot be combined with `--full`, `--source`, or `--field`.

Filtered output reports `matched_sources` and `matched_fields`, so an unmatched source is visible even when the field list is empty. Compact filtered output also reports `omitted_fields` and `omitted_values`. Use `--full` with a filter to inspect all related records and provenance without the limit. That view carries `filtered: true` and `source_pack_digest`. Use the original pack file for import. Unfiltered `--full` returns the pack itself. Inspection is offline and never reads credentials.

### Import

```sh
cqlmatic env import --from snapshot.json --out .local/imported.json
cqlmatic env import --from examples/legacy-observation --environment examples/environment.json --out .local/example-pack.json
```

Required flags are `--from FILE_OR_DIRECTORY` and `--out FILE`. Native files must pass identity/integrity/profile/record validation. `--environment FILE` optionally asserts matching identity for native input and is required for legacy directory input. Choose a **separate output path**. Import refuses a destination that resolves to the native input, environment file, or any legacy component it reads. Use `--replace` to replace a separate existing output. Replacement does not perform a refresh merge. The JSON report identifies the imported path/digest. A durability failure can report `active_pack_updated:true` with exit `2`; inspect the output before retrying. See [import mechanics](docs/ENVIRONMENT.md#import-and-inspect).

### Discover

| Mode | Required options | Additional options / constraints |
| --- | --- | --- |
| Plan only | `--request FILE` | No credentials or search; prints initial probes, deferred stages, budgets. This is the default. Pack/receipt/credential flags do not make it execute. |
| Native API execution | `--request FILE --execute --pack FILE --receipt NEW_FILE` | Add a new `--state FILE` for durable checkpoints; optional protected credentials file, otherwise Falcon environment variables. |
| Native saved run | `--execute --state FILE --resume` | Original target/request and remaining budgets; exact known-job cleanup first when needed. Optional protected credentials file. |
| MCP initialization | `--mcp --request FILE --state NEW_FILE --pack FILE --receipt NEW_FILE` | No `--execute`, credentials flag, or answers. Creates local relay state and emits probes. |
| MCP dispatch | `--mcp --state FILE --dispatch PROBE_ID` | Reserve one exact outstanding probe before its host call. |
| MCP answer/status | `--mcp --state FILE` | Format 3 accepts one matching `--answer PROBE_ID=FILE`; omit answers to poll or recover activation. |
| MCP cancel/resume | `--mcp --state FILE --cancel` or `--resume` | Retain accepted captures; settle any in-flight call before resuming. |
| MCP cancellation settlement | `--mcp --state FILE --settle-cancelled PROBE_ID=FILE` | Protected exact caller-attested terminal cleanup report; does not itself stop a hosted job. |

New MCP initialization uses durable format 3. Dispatch exactly one emitted probe before the host call, then submit its entire original result with the matching ID. Accepted captures replay locally; retries consume attempts and safely paused time does not renew the original allowance. `--cancel`, `--resume`, `--dispatch` and `--settle-cancelled` are separate saved-state controls: do not combine them with initialization or answers. Inventory-owned checkpoints use the inventory lifecycle actions with optional `run_index`; direct `--resume` and `--dispatch` reject them. Direct pause, answer and settlement controls remain available for cleanup. Format-2 checkpoints retain their legacy behavior. See [cancellation and settlement](docs/ENVIRONMENT.md#cancel-and-resume-discovery) for strict host evidence, native Ctrl-C/owner limits and cleanup-pending recovery. Native execution without `--state` keeps its final-receipt workflow.

```sh
cqlmatic env discover --request examples/discover.request.json
```

The example above is a safe offline plan. Live searches require the operator's own explicit target, authorization, permissions, budgets, and RFC 3339 window bounds with explicit offsets. Discovery emits the search window in UTC with `Z`. A pack's searchable `view` differs from a `#repo` selector.

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
| `env discover --mcp`, native saved run | Complete activation | — | Failed/error | Needs probes, paused, cancel/cleanup pending or activation blocked |
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

Follow the [analyst workflow](docs/ANALYST-WORKFLOW.md) and [MCP companion guide](docs/MCP-COMPANION.md). Use full requests for explicit context, retain query bytes, consume the documented output type for each command, inspect the automatically selected inventory or choose an explicit pack, and inspect results after authorized execution. [Packaged agent instructions](integrations/agent/AGENTS-snippet.md) are optional guidance; no skill installation is required for the CLI. `cqlmatic skill install --client CLIENT`, or `--dir DIR` for another host, [installs the agent skill](docs/SETUP.md#install-the-agent-skill) with the guides it links to. It prints the installed path and file counts, exits `0`, and reports a refused target as a `SKILL_TARGET` diagnostic with exit `2`.

Recognized final answers from the hosted search tool end a format-3 MCP attempt as `timed-out`, `not-started` or `failed-after-start`; a completed list that cannot supply the required measurement ends it as `completed-unusable`. Exact terminal bytes and their hash are retained separately from accepted evidence. The run pauses, the attempt stays charged, and an explicit resume is required before another dispatch. Resume emits the same unaccepted probe. A cutoff does not prove the remote job stopped: at most one leftover job per cut-off attempt may overlap later searches. A missing answer or client wait without a tool answer keeps the attempt unresolved and blocks further dispatch. An unrecognized error shape or wording, malformed or incomplete response leaves it in `cleanup-pending`. Resolve that same attempt with an acceptable original successful result or exact bound cancellation report before resuming. Under the retained-run rules, a format-3 MCP checkpoint from 0.1.6 or 0.1.7 with exhausted active time and one distinct final answer can stop blocking other inventory searches, while lifecycle actions require its producing release. See [hosted search outcomes](docs/MCP-COMPANION.md).
