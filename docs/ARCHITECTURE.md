# Architecture

See [application flow](FLOW.md) for the user-facing path and [inventory](INVENTORY.md) for evidence-derived completion. `setup` and `inventory` contain pure contracts and decisions; `setup_cli` and `inventory_cli` own terminal/file/native-client/API adapters. The host model supplies reasoning; these modules do not invoke a model. User maps start empty and query generation receives an explicit pack.

CQLmatic is a modular monolith in one Cargo package. The executable is the supported integration surface. Internal Rust modules are implementation details, not a promised public SDK.

## Dependency and ownership boundaries

| Module | Responsibility | I/O |
| --- | --- | --- |
| `setup` | Strict named-native or external selection, preview plans, phases and saved discovery evidence | None |
| `setup_cli` | Native-client detection/commands, guarded Copilot configuration writes, external report import, protected setup state/backups and terminal flow | Filesystem/native processes |
| `inventory` | Strict inventory actions, source/parser review, conservative declarations and completion assessment | None |
| `inventory_cli` | Inventory/status adapters, parser API reads, saved evidence import and cumulative-pack activation | Filesystem, parser API, relay adapters |
| `contracts` | Strict versioned requests, semantic plan, result types | None |
| `language/english` | Full-consumption bounded grammar to plan | None |
| `language/plan` | Recipes and typed plans to CQL AST | None |
| `language/ast` | CQL structure, escaping, canonical rendering, spans | None |
| `language/parser` | Context-sensitive, budgeted CQL parsing | None |
| `language/validate` | Profile rules, signatures, effects, lints, conservative regex syntax | None |
| `catalog` | Bundled reference records and executable function profile | Embedded data only |
| `environment` | Pack contracts, identity and integrity checks, evidence-preserving merge | None |
| `app` | Generate/check use cases and source-aware grounding | None |
| `assess` | Post-execution checks of a saved MCP result against query and optional pack | None |
| `mcp` | Normalize saved MCP results; orchestrate the agent-relayed discovery state/answer loop | Local files for relay state and saved answers; injected search replay |
| `storage` | Protected regular files, persistent writer lock, atomic snapshot commit | Filesystem |
| `network` | Fixed Falcon endpoints, OAuth, deadlines, job lifecycle | HTTPS and credential reads |
| `discovery` | Bounded measurement orchestration through `SearchBackend` | Injected search boundary and timestamps |
| `main` | CLI adapters, JSON/JSONL framing, stdout/stderr, exit codes | CLI/filesystem |

The pure core receives explicit data. It never reads credentials, chooses an endpoint, obtains the current time, or refreshes a pack. A pack can supply evidence but cannot install executable rules.

## Generation flow

1. Decode the strict v1 contract. Unknown request fields and unsupported schema versions fail explicitly.
2. Normalize a recipe, plan, or English request into the same semantic `Plan`.
3. Lower typed predicates and stages into the CQL AST. Literal, glob, regex, missing-value policy, numeric versus lexical ordering, exactness, and join match caps remain distinct.
4. Render with one escaping implementation, then parse the complete generated text.
5. Check the versioned profile and collect advisory diagnostics.
6. Assess source-specific grounding from optional snapshot evidence.
7. Return separate interpretation, validation, grounding, and outcome fields. Emit a query only for `ready` generation.

No runtime fallback sends input to a model. No automatic router switches between differently interpreted intents.

## Syntax and assurance

The parser separates filter syntax from expression syntax: `field=value` is distinct from `test(left==right)`, and a slash can be regex syntax or division depending on context. Boolean rendering is parenthesized. CQL's OR-before-AND rule is preserved when checking existing text; English with mixed operators requires parentheses.

The checker enforces a bounded profile, not full platform compilation or type inference. Scalar computation, nested aggregates, source replacement, correlation branches, and pipeline functions have different contexts. Conservative regex parsing never substitutes Rust regex matching for CQL semantics.

`ready` is a local contract result. Execution is a separate concern. Runtime feature availability, resource ceilings, data absence, and analyst intent beyond the bounded grammar remain explicit limits.

With a pack, unresolved `record_kinds` facets produce informational `CQL_RECORD_KIND_NOTICE` in ordinary mode. Strict observed mode requires kind context or explicit acceptance, retaining `CQL_RECORD_KIND_AMBIGUOUS` as an error or accepted warning. Packs do not add language semantics. `assess` checks the saved MCP result, query and optional pack offline, reporting `no-findings`, `advisory`, `inconclusive` or `error`. Even `no-findings` does not attest completeness or query correctness. See [query reference](QUERY-REFERENCE.md) and [result assessment](RESULT-ASSESSMENT.md).

## Snapshot lifecycle

One native JSON file holds an entire pack. Its SHA-256 covers metadata, identity, and records, excluding the digest field itself. This detects corruption and accidental cross-context reuse; it is not an authenticity signature.

A separate persistent lock file is never unlinked by a normal writer. For authenticated API discovery, the pack writer lock spans the run: it is acquired before reading the predecessor and remains held through measurement, merge, and activation. MCP relay does not hold a lock while the agent performs searches: each init/step call holds its state writer lock, and takes pack/receipt locks at initialization or activation (including activation recovery). Writers take locks in state → pack → receipt order. The frozen predecessor and activation intent detect intervening pack changes; a conflict is reported instead of silently replacing unrelated work. Readers continue using the committed snapshot. A writer validates a candidate, writes a new same-directory temporary file, syncs it, renames it over the active file, and syncs the directory. Failures before rename preserve the old snapshot. A post-rename durability failure is reported; the new complete file can already be visible.

Merge requires identical cloud/account/view and a newer snapshot time. Completed selected sources must preserve compatible scope restrictions. Source and field updates also check actual measurement ordering; a fresh wrapper cannot promote backdated, tied or unorderable contradictory evidence in either direction. Retained negative or stale source evidence requires a newer qualifying source measurement for recovery; undated negatives and stale markers require a window starting after the retained snapshot. Bounded population batches preserve untargeted records and dates. Retaining prior source proof also preserves omitted fields; explicit field measurements retain their own windows and ordering. Untargeted sources, quiet historical fields, old samples, original declaration dates, parser contracts, and operator annotations remain. Refresh does not turn an old declaration into a new review.

Agent-facing brief output is a separate bounded projection of committed status: fixed vocabulary, counts and opaque references, never operational payloads. Status and inventory actions serialize the complete brief response before writing it under one 4,096-byte limit. Notification snapshots commit to the exact environment and all unacknowledged work revisions; acknowledgement recomputes that content under the existing writer before changing only notification bookkeeping. Generation/checking, imports and status reads do not attest user delivery. The foreground host communicates findings before explicitly acknowledging them. Full execution descriptors and original results remain protected local data.

Inventory state is separate from setup state and environment packs. Setup records named-native or external progress; it cannot implicitly select query identity or load a pack. Native Claude/Copilot configuration checks bind the selected local entry, while a selected-session report supplies the effective OAuth identity as caller evidence; external import binds a report without inspecting host configuration. Inventory stores scouting captures, target/context reviews, parser catalog and exact-byte hashes, bindings, exclusions, discovery runs, and references to retained pack snapshots. Parser declaration extraction is lexical and conservative; unsupported/conditional lines require explicit snapshot-bound semantic review. Neither declaration extraction nor a review asserts field presence.

Inventory builds a cumulative active pack from successful source/field runs. Historical snapshots remain retained; current completion uses the active pack and current workflow evidence, with reconciliation bound to an evidence fingerprint. A complete selected source or an imported pack does not establish full-environment completion. Status is offline and validates referenced pack identities and digests. `covered_queries_usable` is a workflow allowance, not a per-query proof. See [inventory](INVENTORY.md) for exact completion requirements and [contracts](CONTRACTS.md) for output shapes.

The CLI reads protected inventory metadata before selecting evidence per request or JSONL row. Full and Quick can supply a matching active pack and otherwise unspecified target. An explicit pack stays primary; matching inventory supplies grounding safeguards. Foreign inventory is skipped before opening its active file. Protected reads verify existing storage without creating missing directories. Matching active-file failures remain blocking even under lenient grounding. Assessment shares pack selection while excluding ledger-to-rate inference and lead writes; only Quick generation/checking may append unverified leads. Pure engine modules receive these inputs without filesystem access.

In MCP mode, an agent invokes the emitted read-only `falcon_search_ngsiem` probes and relays the saved result bytes. The local state is unkeyed: its digest and consistency checks detect corruption and inconsistent edits, not a party able to rewrite it consistently. The agent is trusted as the relay; the MCP result does not attest search-job completeness. The authenticated API discovery path remains separate. Inventory holds its own writer lock while an action updates inventory state; that does not extend the lower-level MCP pack lock across separate agent relay calls.

## Cloud boundary

Endpoints are fixed for `us1`, `us2`, and `eu1`; TLS verification stays enabled and redirects are disabled. Credentials are read from protected local files or explicit environment variables and tokens remain in memory. API-client fingerprints bind credentials, cloud, and optional member CID without pretending to be a verified CID.

Searches are finite, use explicit absolute UTC windows, disable event skipping, and have query, poll, response-size, row, retry, and wall-clock budgets. Absolute times are sent as integer epoch milliseconds. GET retries honor bounded `Retry-After` delays. Start POSTs are not repeated after ambiguous transport failures; one explicit 401 permits token renewal.

Poll snapshots replace earlier snapshots. A done flag alone does not establish completion: cancellation, errors, warnings, skipped/partial/truncated flags, malformed rows, and result caps prevent complete-evidence attestation. Known job IDs receive a separate bounded cleanup attempt on every normal return path, including cancellation. Forced termination or power loss can prevent that attempt; an ambiguous start can leave no usable job ID.
