# Environment packs and discovery

Basic `generate` and `check` work offline without a pack. Use [setup](SETUP.md) for the authorized search host, then [inventory and status](INVENTORY.md) when the task needs source evidence. Parser inventory adds possible mappings and transformation context to observed schema; the assistant reviews those semantics. The sections below cover lower-level pack import and discovery. Use the [documentation index](README.md) to choose a guide, [contracts](CONTRACTS.md) for JSON shapes and [troubleshooting](TROUBLESHOOTING.md) for diagnostic recovery. Every product install starts without tenant references.

Generation does not need Falcon credentials. A pack is optional grounding evidence. `generate`, `check` and `assess` reuse a matching confirmed local inventory's active pack in Full or Quick mode. `--inventory-state FILE` selects another protected inventory, and `--pack FILE` keeps an explicit snapshot primary. Full and Quick can fill otherwise unspecified target context; explicit request and pack identities still take precedence. Foreign inventory is skipped before opening its active snapshot. A broken matching snapshot blocks use even in ordinary mode, while a missing default inventory leaves basic commands available without creating storage.

Use `--require-observed` when referenced deployment fields must have qualifying evidence. A validated saved-query ledger from the selected inventory can supply caller-attested positive fields for its exact source, target and window, including without a pack. Other unobserved fields still block. Later or unordered contradictory pack measurements and strict record-kind safeguards remain applicable. Assessment uses pack rates without deriving rates from the ledger. Strict acceptance does not independently prove that a saved response came from the claimed query or certify source availability: a source-only count has no deployment field to ground.

Grounding qualifies source dependencies, including an incoming target value that a nullable assignment or transform may retain. It does not promise presence on every event. An explicit projection or drop can establish that a target was absent; CQLmatic never inserts those operations. Ordinary mode exposes conditional lineage without blocking generation. Strict mode also requires evidence for a retained incoming target. Default input fields and independent or external sources keep their own context.

Here, current means observed in the selected snapshot rather than marked historical. There is no wall-clock expiry policy. Generation/checking do not choose an execution window. Inspect observation windows and original provenance dates before deciding whether evidence is recent enough for the intended search.

## Use the inventory for reasoning

An environment pack is a local JSON snapshot that analysts and agents can inspect independently of `generate` or `check`. It can supply context for choosing datasets, finding candidate correlation fields, or reviewing coverage gaps throughout an investigation.

The agent needs the pack's location and permission to run the CLI or read the file. Start with `cqlmatic status --json --summary` to find the active path and relevant gaps. Replace `PACK_PATH` below with that path and `SOURCE_ID` with a source ID from the overview:

```sh
cqlmatic env inspect PACK_PATH --summary
cqlmatic env inspect PACK_PATH --source SOURCE_ID
cqlmatic env inspect PACK_PATH --full
```

`--summary` returns source selectors/counts and snapshot metadata without field/value rows. It is an overview and cannot be combined with `--full`, `--source` or `--field`. Request focused source/field records separately and reserve `--full` for tasks that need the entire snapshot. Existing inspection calls keep their default shape. Source or field filters return matching field records, population evidence, and stored value samples. `--full` returns the entire pack, including provenance and annotations. These are offline reads; inspection does not refresh the snapshot or run a search.

Discovery does not automatically populate the model's context or create persistent agent memory. The agent must retrieve relevant records when needed. Automatic inventory reuse and explicit `--pack` selection supply evidence to CQLmatic's checks; they do not pass the whole inventory to the agent. An integration that exposes only generation and checking must separately allow inspection or file access for this reasoning workflow.

Packs remain scoped, sampled evidence. Parser declarations, documented standard fields, and observed fields are distinct. Inspect observation windows and provenance before relying on them; neither inspection nor `--require-observed` establishes live source availability, complete results, or successful execution. Keep packs containing sensitive field names and sampled values in protected local storage.

## Bind an account

Environment identity consists of `cloud`, `tenant`, and `view`. Supported clouds are `us1`, `us2`, and `eu1`. `view` is the explicit searchable API target. Supply a confirmed 32-hex CID, or derive an API-client fingerprint locally:

```sh
./target/release/cqlmatic env identity --cloud us2 --view CONFIRMED_VIEW --credentials-file PATH_TO_PROTECTED_CREDENTIALS
```

This command reads credentials and prints only an `EnvironmentId`; it makes no network request. The `api-client:<SHA-256>` binding identifies the credential account configuration, not a provider-verified CID. Changing the client ID, member CID, or cloud changes the binding. Secret rotation with the same client ID preserves it. A CID supplied by an operator remains an operator assertion when OAuth exposes no CID.

Store the output in a protected local JSON file and copy the identity into your discovery request. Do not use the all-zero illustrative identity in `examples` for execution.

## Credentials

Use a regular, non-symlink credential file. On Unix, its mode must be `0600` or stricter; newly created pack and receipt files use `0600`, and their newly created storage directories use `0700`. On Windows, protect external credential files and lower-level import/discovery outputs with operator-managed ACLs. The setup and inventory workflows separately create or verify dedicated private state directories, as described in [the security policy](../SECURITY.md); this does not extend ACL enforcement to arbitrary input/output paths. Accepted credential formats are JSON, `key=value`, or a label followed by its value on the next nonempty line. Recognized labels are `client_id`, `client_secret`, optional `base_url`, and optional `member_cid`.

An approved HTTPS Falcon cloud root is the only accepted `base_url`. It must match the explicit environment. The client does not accept an arbitrary authenticated endpoint. With no file, the explicit variables are `FALCON_CLIENT_ID`, `FALCON_CLIENT_SECRET`, optional `FALCON_BASE_URL`, and optional `FALCON_MEMBER_CID`.

Never pass secrets as CLI arguments. Keep credential files and query receipts outside tracked source. The query-job lifecycle needs NGSIEM READ and WRITE: `StartSearchV1` uses POST with WRITE, `GetSearchStatusV1` uses GET with READ, and `StopSearchV1` uses DELETE with WRITE. READ alone cannot run this lifecycle. See the [CrowdStrike NGSIEM API reference](https://developer.crowdstrike.com/api-reference/collections/ngsiem/). Parser catalog and definition reads require the separate NGSIEM Parsers: READ scope. A 403 leaves that evidence unavailable; record the gap and continue only with supported contract or observation paths.

## Plan, then execute explicitly

Make a local copy of [the synthetic example request](../examples/discover.request.json), set the confirmed account/view, and choose an authorized absolute UTC window. `environment.view` selects the searchable target; source `repo`, `type`, or both select data within that target. Never replace a confirmed target with `search-all` merely because an example uses it. Select sources explicitly, or set `census=true` to discover repositories/types observed within that window.

```sh
./target/release/cqlmatic env discover --request .local/discover.json
./target/release/cqlmatic env discover --request .local/discover.json --execute --pack .local/pack.json --credentials-file PATH_TO_PROTECTED_CREDENTIALS --receipt .local/discovery-001.json
```

The default command only prints initial probes, deferred stages, and budgets. Execution requires `--execute`, an output pack, and a new receipt path. Discovery uses search jobs; it does not ingest events or modify parsers, rules, dashboards, lookups, or tenant configuration.

### Discovery request reference

The strict version-1 request rejects unknown properties. Inspect `cqlmatic --schema discovery` and the [contract reference](CONTRACTS.md) before constructing one.

| Property | Default and constraint |
| --- | --- |
| `schema_version` | Required; `1`. |
| `environment` | Required `cloud`, `tenant`, `view` binding; must match an existing destination pack. |
| `start`, `end` | Required increasing absolute UTC timestamps ending in `Z`; at most seven days. |
| `census` | `false`; adds repositories/types observed in the full window to explicit sources. |
| `depth` | Omitted or `full`: the normal field measurement graph. `sources-only`: volume checks only; field/value/contract options must be empty. |
| `sources` | `[]`; select at least one source unless `census: true`. Each scope requires nonempty `repo` or `type` (or both), and forbids `event`. Duplicate selectors fail. At most 100 sources after census. |
| `kind_fields` | `[]`; additional validated discriminator fields, supplementing automatic candidates. |
| `value_fields` | `[]`; unique validated fields for up to 25 sampled values per field/source in the recent sample window. These are samples, not exhaustive enums. |
| `max_population_fields_per_source` | Omitted or `null`: population selection from the sampled fieldset. Integer `1..4000`: enables stratified discovery and caps selected population fields. |
| `population_fields` | `[]`; source-bound explicit population batches, described below. |
| `contracts` | `[]`; `{source, contract}` parser declarations. Each source must exactly match an explicit `sources` entry; census alone cannot bind a declaration. |
| `max_queries` | `200`; integer `1..1000`, shared by all sources and stages. |
| `max_seconds` | `900`; integer `1..3600`, shared wall-clock budget. |

A `fieldset()` measurement may contain at most 4000 distinct fields. A population cap does not bypass that fieldset limit, source limit or overall job/time budget. Dry-run output contains `schema_version`, `environment`, `initial_probes`, `deferred_stages`, budgets, `is_live: false` and `executes_searches: false`; dependent probes require earlier results and are not all available initially. API execution reports `status`, `active_pack_updated`, `captures` (probe, retrieval time and job receipt) and `diagnostics`; see [contracts](CONTRACTS.md) for the distinction from MCP step output.

At the default `full` depth, a run measures source volume and dataset facets, samples `fieldset()`, counts field population in chunks with matching denominators, probes known tag candidates, and optionally samples requested values. Source volume, dataset facets and record-kind facets use the full requested window; source evidence and its provenance retain that window. A full-window census is an observed inventory within that window. It cannot identify quiet or inaccessible sources or certify the entire tenant.

Each run permits at most seven days of search time, 1–1000 query jobs, 1–3600 seconds, 100 sources, and 4000 fields per fieldset measurement. Initial field sampling uses the final 15 minutes of the requested window, or the entire window if shorter. Broader or capped runs need an explicitly narrowed request; no automatic shard aggregation is performed.

Field `coverage.rate` is a fraction from 0 to 1: populated-field count divided by its matching population-query count in the same source scope and window. The denominator comes from that population query, rather than a separate volume or kind-facet count. The pack stores the fraction, not the actual denominator; retain the protected discovery receipt when the counts matter.

### Bounded stratified discovery

Set the optional version-1 request property `max_population_fields_per_source` to an integer from 1 to 4000 to enable bounded stratified discovery through either API execution or the MCP relay. For example, `"max_population_fields_per_source": 128`, `"max_queries": 40`, and `"max_seconds": 3600` bound a single-source run. Omission or `null` retains the existing uncapped population selection and probe graph; existing requests need no changes.

Bounded discovery finishes tag checks and full-window kind facets before selecting population fields. It chooses the first nontruncated discriminator with more than one value: observed requested `kind_fields` in request order, then `#event_simpleName`, `#type`, `#event.kind`, `event.kind`, `#event.module`, and `#event.dataset`. The full-window dataset facet supplies the last candidate without another query. A 50-row record-kind facet is marked truncated and cannot supply strata.

For that discriminator, discovery selects at most ten values with positive counts of at most 50,000 events, ordered by ascending count and then value. Each selected value gets a full-window, literal-filtered `fieldset()` probe. The renderer escapes literal values, including `*`; no raw filter fragment is accepted. Fields found by these probes join the recent sampled fields as candidates.

Population selection uses three tiers, with lexical ordering within each: directly observed requested kind/value fields, newly discovered fields from the rare-kind probes, then existing sampled fields. The configured cap limits population fields. A cap too small for the observed requested fields fails the run. Direct tag/kind evidence may add metadata fields without population rates. Fields only listed by `fieldset()` are documented namespaces, with no current populated-field proof and a null rate; separate declarations and actual tag/kind/population measurements retain their own evidence. A capped refresh preserves previous records and dates outside the fields actually measured. A new listing does not replace an earlier positive or negative measurement. Explicit parser declarations retain their separate declaration evidence.

Existing sampled fields keep source-wide population measurements over the final 15 minutes. Newly discovered selected fields get source-wide population measurements over the full request window. Those queries contain no kind filter: a kind's fraction of source volume is never used as field coverage. Each field and its provenance retain the corresponding population window. Population probes measure at most 20 fields each; splitting recent and full-window fields requires at most eight population probes for a 128-field cap. With the standard candidates, three base probes, nine tag checks, up to five kind facets and ten rare-kind fieldsets fit the 40-query single-source budget. Additional requested discriminators or value facets consume that same budget.

Malformed dataset or kind facets, invalid counts, unrepresentable discriminator strings, timeouts, incomplete results and exhausted budgets fail closed. Dataset rows must carry a string `#event.dataset` and an explicit nonnegative integer `events`; rows are never silently discarded. No automatic sharding or partial replacement follows a failure.

`value_fields` opts into bounded value facets. Values are stored as samples, with their field identity and provenance. A requested field absent from one selected source may return an empty facet without aborting other source measurements; malformed nonempty results still fail. A top-value facet does not certify an exhaustive enum or a globally exact ranking. Packs and receipts can contain sensitive field names and values, so keep them protected.

Discovery automatically probes generic record-kind candidates (`#type`, `#event.module`, `#event.kind`, `event.kind`, `#event_simpleName`) when present. Source relationships retain sampled `record_kinds` facets; these do not prove an exhaustive type inventory. If a source uses another discriminator, add it to the request's `kind_fields` array and refresh the pack. `kind_fields` supplements the generic candidates without changing query semantics. Ordinary queries spanning multiple or incompletely sampled kinds continue with informational `CQL_RECORD_KIND_NOTICE`. With `--require-observed`, unresolved kinds produce `CQL_RECORD_KIND_AMBIGUOUS` and require context: select one exact kind, use the applicable event selector, or group by kind before field-changing/aggregating steps. Explicitly accepted strict mixing retains a warning via `--accept-mixed-record-kinds` or `context.accept_mixed_record_kinds`.

### Explicit population batches and targeted refresh

Use `population_fields` when particular fields need population measurements rather than automatic selection. For example, this request fragment targets one explicitly selected synthetic source:

```json
{
  "sources": [{"repo": "synthetic_repo", "type": "a"}],
  "population_fields": [
    {"source": {"repo": "synthetic_repo", "type": "a"}, "fields": ["user", "#type"]}
  ]
}
```

This is a fragment, not a complete request. Each selection must match an entry in `sources` exactly, including `type`. Census-discovered sources alone are insufficient. Select each source at most once; its nonempty field list must contain unique valid field names, at most 4000 and no more than the configured population cap. `#field` and `field` are separate namespaces.

With no stratification cap, this list replaces that source's automatic population selection. With a cap, tag checks, kind facets and rare-kind fieldsets still run; the explicit list replaces tier-based selection and must include every directly observed requested `kind_fields`/`value_fields` field. Fields found in the recent sampled fieldset use the final 15 minutes (or shorter request window); selected fields not found there use the full request window, even if discovered by a rare-kind fieldset. All population counts remain source-wide with a matching denominator, never the discriminator-filtered count. Population queries contain at most 20 fields each. Tag checks and value samples keep their recent windows; kind facets, source volume and dataset facets retain the full window.

Source `evidence.measured_fields` records the exact population batch measured by a run. Existing fields outside that batch retain their records and original evidence dates unless a separate qualifying auxiliary measurement updates that exact field. A field actually measured as zero remains current negative evidence, including auxiliary measurements with a null population rate. Newly listed fieldset namespaces can be retained without a population rate or observation claim. A general source refresh without a measured-field batch can mark previously known fields absent from its candidate as historical. Inspect field provenance and observation windows separately from source-wide evidence.

## MCP discovery

If an authorized agent has the read-only `falcon_search_ngsiem` MCP tool, it can relay discovery probes without supplying Falcon API credentials to CQLmatic. Keep the request, resumable state, pack, receipt and **raw** MCP responses in protected, untracked storage. Start with an explicitly bound request, authorized absolute UTC window, selected sources or census and finite budgets:

Discovery inventories whole repository, type-only or repository/type sources; it does not accept event-scoped sources (a single `#event_simpleName` inside a repository). Very large multi-event repositories can list thousands of fields in the sample window, requiring many population probes in uncapped mode. Use bounded stratified discovery to select a finite population sample, narrow by type, or consult the bundled standard field catalog (`cqlmatic catalog --standard cps|ecs`; predictions, not tenant observations).

Answer files are parsed fail-closed: error, timeout and completeness indicators (including inside `metaData`/`metadata`), unknown top-level keys, non-object rows, duplicate JSON keys and results of 10,000 rows or more all end the run with `failed` and leave the working pack unchanged.

```sh
cqlmatic env discover --mcp --request .local/discovery.json --state .local/state.json --pack .local/pack.json --receipt .local/receipt.json
cqlmatic env discover --mcp --state .local/state.json --answer PROBE_ID=.local/probe-result.json
```

The first call initializes the loop; for each `needs-probes` response, invoke only the returned `falcon_search_ngsiem` arguments and **save the result exactly as received before `--answer`**, without edits. If the server requires tool discovery first, follow its contract. Repeat until `complete` activates the pack (exit `0`), `needs-probes` or `activation-blocked` requires attention (exit `3`), or `failed` reports an error (exit `2`). Do not reinterpret a timeout as an empty result. Provenance uses `mcp-observation` with `observed-only` coverage; CQLmatic did not start or stop these MCP jobs and cannot attest result completeness. The [MCP companion guide](MCP-COMPANION.md) covers the agent's construction, execution and post-execution handoff. The authenticated API path above remains available.

## Declarations and observations

An optional `contracts` entry binds a supplied parser contract to an explicitly selected source. It records declared fields, tag conversion, arrays, original retrieval date, and optional source hash. No parser assignment is guessed from a similar name. Available parser metadata can be read explicitly:

```sh
./target/release/cqlmatic env parsers --environment .local/environment.json --credentials-file PATH_TO_PROTECTED_CREDENTIALS --limit 50
./target/release/cqlmatic env parsers --environment .local/environment.json --credentials-file PATH_TO_PROTECTED_CREDENTIALS --id PARSER_ID --template
```

Field namespaces remain distinct. A data field `event_simpleName` does not establish a tag `#event_simpleName`. Discovery uses `unknown` for unmeasured types and `null` for undeclared array shape. A field listed by `fieldset()` can still have zero population in the matched sample. Declarations, field listing, and populated observations stay separate.

## Import and inspect

```sh
./target/release/cqlmatic env import --from .local/snapshot.json --out .local/pack.json
./target/release/cqlmatic env import --from PATH_TO_LEGACY_CORPUS --environment .local/environment.json --out .local/imported.json
./target/release/cqlmatic env inspect .local/pack.json --field user --source example_repo
```

Native imports validate format, profile, identity, digest, record shapes, and references. Legacy import accepts the pinned NG-SIEM JSONL records from a directory containing `sources.jsonl`, `fields.jsonl`, `provenance.jsonl`, and optional `values.jsonl` / `annotations.jsonl`, or its `environment` child directory. An explicit environment binding is required because legacy files do not carry the native account identity.

Choose an output path separate from the native input, environment file, and every legacy corpus component. Import reads the inputs before writing the selected destination, but does not enforce input/output separation. `--replace` can overwrite whichever existing output you choose, including an input or a legacy `sources.jsonl` in the imported directory. Inputs remain untouched only with a separate output; do not point `--out` at a source component. Imported declared-only or historical records do not become observed evidence.

Source names are display labels and may equal a selected type. The source's `selectors.repo` and `selectors.type` identify its repository/type scope. `env inspect --source` matches an exact source ID, display name, or repository selector; it does not directly match the type selector. Prefer the source ID to avoid ambiguous labels.

The inventory workflow's `import-pack` action adds compatible historical records and registers valid selectors as unconfirmed sources. Repeated identical records are idempotent, older disjoint history is allowed, and differing records under the same ID reject atomically. This import is distinct from a measured refresh: imported history does not reopen or rewrite statistical evidence and cannot establish missing workflow completion.

The persistent inventory supports Full mode by default and an opt-in Quick mode selected through Settings or `set-discovery-mode`. Returning to Full retains existing packs, source/parser records, leads and saved-query observations while reevaluating Full requirements. Quick scouting requires an explicit chosen view or `search-all`, an authorized UTC window, and a one-query host budget of `max_seconds: 1..600`. Its budget is emitted separately from MCP arguments and must be enforced by the host; CQLmatic cannot enforce a relayed scout deadline. Discovery then uses a confirmed selected-source target and finite query/time limits checked by the CLI during relay steps. Sources-only performs volume checks; sources-and-fields uses a 20-field population cap with the existing stratified measurements. Its completion flag is limited to the chosen scope/depth. See [inventory mode and evidence details](INVENTORY.md#full-and-quick-discovery).

Basic accepted queries in confirmed Quick mode can save source/input-field leads without a pack or search. `--inventory-state FILE` chooses a protected Full or Quick state for `generate`, `check` and `assess`; the default is the local inventory. `CQL_QUICK_TARGET` identifies supplied environment context in either mode, and `CQL_QUICK_LEADS` identifies new Quick suggestions. Full mode and assessment never capture leads. Leads remain unverified and do not change observation status or complete discovery.

The separate `import-query-observations` inventory action accepts protected exact query/result files and the caller's explicit query/result/target/window assertion. It records direct positive field counts with hashes and original-file references; it copies no raw bytes into state and does not rewrite the pack. Derived/aliased/aggregate/join results, incomplete responses and empty evidence are rejected; `0` and `false` remain positive values. From a selected confirmed inventory, the validated ledger can support strict grounding without a pack for its exact positive fields, source and target. It cannot supply unrelated fields, override later or unordered contradictory pack measurements, or remove strict record-kind requirements. Both modes can fill missing target context while respecting explicit identities. The caller assertion is not independent proof of execution or provenance. See [saved-query observations and unverified leads](INVENTORY.md#saved-query-observations-and-unverified-leads) for the action and limitations.

### Offline observation example

The three files in [examples/legacy-observation](../examples/legacy-observation) contain one synthetic source, one data field `user`, and one provenance record. The all-zero identity in [examples/environment.json](../examples/environment.json) is illustrative. These commands are an offline import and check demonstration, with no tenant contact:

```sh
./target/release/cqlmatic env import --from examples/legacy-observation --environment examples/environment.json --out .local/example-observation.json
./target/release/cqlmatic env inspect .local/example-observation.json --source env:repo:example_repo --field user
./target/release/cqlmatic check '#repo=example_repo | user=* | count()' --pack .local/example-observation.json --require-observed --json
```

For operational use, create protected local input files with your explicit environment identity and actual source selectors, field namespace, and observation evidence. Preserve original retrieval dates and bounded observation windows. Use `inventory_status: "observed-only"`, source `evidence.observed: true`, field `evidence_kind: "observed"`, and field `coverage.observed: true` only when the underlying evidence supports them. The example's labels and synthetic evidence do not establish anything about a real deployment.

The provenance validator accepts these values:

| Property | Accepted values | Values that can qualify as deployment observation evidence |
| --- | --- | --- |
| `source_type` | `official-docs`, `mcp-observation`, `console-export`, `ngsiem-api`, `manual-curation` | `mcp-observation`, `console-export`, `ngsiem-api` |
| `coverage_status` | `complete`, `partial`, `declared-only`, `observed-only`, `reconciled`, `stale`, `unavailable`, `incomplete`, `error` | `complete`, `observed-only`, `reconciled` |

Strict grounding needs at least one qualifying type/status pair among the provenance references of **both** the selected source and the matched field. A qualifying record may coexist with retained stale or nonqualifying records. Source inventory must be `inventoried`, `observed-only`, or `reconciled`, with no explicit `evidence.observed: false`. The field must be observed, with no explicit `coverage.observed: false`. Historical `not-reobserved` fields and stale sources remain ineligible; source selectors, tag/data namespaces, array shape, and unambiguous field matching still apply.

There is no age cutoff: retrieval dates are preserved and the operator judges recency. Import validates record structure and references, not whether an observation occurred. Changing labels cannot create evidence or turn a declaration into a deployment observation.

## Refresh and failure behavior

Authenticated API execution holds the pack writer lock from preflight through measurement and activation. MCP discovery holds its state lock for each CLI invocation; initialization and activation also acquire pack/receipt locks, but the pack is not locked while the agent runs outstanding probes. A competing writer can report `PACK_BUSY`; readers can continue inspecting the current file. At MCP activation the candidate merges with the then-current same-environment pack, before the activation bytes are frozen. Completed selected sources merge into the snapshot. Untargeted records, annotations, parser dates, and historical values remain. In a general source refresh, a previously known field absent from the candidate is marked `not-reobserved`, not deleted or presented as newly observed. A bounded `measured_fields` batch instead preserves existing fields outside its measurements. A `sources-only` run records optional `evidence.discovery_depth: "sources-only"` and preserves all earlier fields and values; volume evidence cannot replace field evidence.

Activation still requires a snapshot newer than its predecessor and also checks the measurements' own windows and retrieval dates. A newer snapshot timestamp cannot promote backdated, tied or unorderable contradictory evidence. Source and event-family restrictions must also remain compatible. If old negative or historical evidence has no orderable measurement, recovery needs an actual new measurement whose window starts after the retained snapshot date. Null or missing source observations cannot clear retained negative or historical source evidence. Ordering conflicts reject activation without replacing the previous pack; changing metadata alone cannot supply proof.

An empty census, quiet source, missing count, timeout, cancellation, warning, truncation or exhausted budget prevents candidate activation. API failures after search execution begins are recorded in the reserved receipt; credential/client preflight failures remove the empty reserved receipt. API activation or receipt-write failures require inspecting `active_pack_updated` and diagnostics because replacement may already be visible. MCP evidence failures persist terminal `failed` state with the diagnostic and accepted answers, with no receipt before activation. Initialization or file I/O errors can occur before a failed state is persisted. Start a failed run again with new state and receipt paths after correcting the cause; do not edit state or replay the whole batch blindly. See [MCP recovery](MCP-COMPANION.md#relay-a-bounded-discovery-run) and [troubleshooting](TROUBLESHOOTING.md).
