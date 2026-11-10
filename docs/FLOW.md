# How CQLmatic works

Basic generation and checking are offline and work without a pack or completed inventory. The assistant supplies intent and identifiers; CQLmatic constructs or checks supported CQL and reports the available evidence. Setup and inventory support authorized search and grounding when needed.

```text
USER + AI ASSISTANT
  Intent, source purpose, parser interpretation, query refinement
        |
        v
CQLMATIC SETUP
  Choose agent/cloud -> native Codex CLI or VS Code Local adapter
                     -> Claude Code or Copilot native adapter, or guided external agent
  Native: detect -> supported registration -> preview -> confirm -> host consent
  Host-managed: configure/sign in within host (no CQLmatic config writes)
  Both: matching caller-supplied tool-discovery report
        |
        v
OPTIONAL ENVIRONMENT INVENTORY  <------------- status --json --summary
  |
  +-> Persistent mode: Full (default) or Quick
  |     Accepted queries -> bounded unverified source/input-field leads
  |     Quick: selected sources -> sources only / sources + fields
  |     Explicit chosen view or search-all + UTC window + finite host budget
  |     Switching to Full retains evidence and opens Full requirements
  |
  +-> CrowdStrike MCP scouting (one authorized query; host-enforced deadline)
  |     Sources + selectors + volume + record kinds + time window
  |                      |
  |                      v
  |     Confirm target + expected sources + purpose/access boundaries
  |
  +-> Falcon Parsers API (separate read-only API client)
  |     Catalog -> definitions -> hashes/dates -> fields/tags/arrays
  |                      |
  |                      v
  |     Model reviews transformations; confirm source/parser bindings
  |
  +-> Bounded source + field discovery through existing MCP client
        Explicit windows -> finite probe batches -> saved answers
        Parser declarations guide additional field measurements
                         |
                         v
                  RECONCILE + SAVE SCOPED EVIDENCE
                  Quick scope / selected scope / Full map completion
                  Observed / declared / historical / pending
                         |
                         v
USER REQUEST -> AI INTERPRETS -> CQLMATIC CONSTRUCTS + CHECKS -> CQL
                                  ^                            |
                                  | optional explicit evidence v
                                  +------------------- AUTHORIZED MCP SEARCH
                                                       |
                                                       v
                                             INSPECT + ASSESS SAVED RESULT
                                             refine, clarify, or hand off
                                                       |
                                                       v
                                             EXPLICIT OBSERVATION IMPORT
                                             Caller-attested exact query/result/target/window
                                             Protected hashes + positive field counts
                                             No raw copy or silent pack update
```

The model reasons; the Rust engine applies typed contracts and deterministic rules. CQLmatic does not run its own LLM or replace your MCP client. The selected host owns OAuth, tool selection, and search execution. Codex, VS Code Local, Claude Code and Copilot surfaces have native setup adapters; other capable agents use tracked external setup. A saved host report is caller-supplied evidence, not independent authentication or configuration attestation. CQLmatic also supports explicit native Falcon API discovery for operators who use that path.

Use [setup](SETUP.md) for the search host and [inventory](INVENTORY.md) when source evidence is needed; use [contracts](CONTRACTS.md) for machine requests. Start with `status --json --summary`, then retrieve relevant full status or focused pack records. Status is an offline checklist and does not refresh authentication or execute searches. `full_map_complete` is derived from the confirmed full target, applicable source/parser reviews and measurements, an active pack, and current reconciliation. Quick's `quick_scope_complete` and Full selected-source `selected_scope_complete` remain distinct from `full_map_complete`. Sources-only Quick work records source presence; sources + fields adds a capped field sample. Parser review and wider source/field coverage stay pending. The saved mode is reused, and switching to Full preserves collected evidence.

State and packs are local snapshots, dated and scoped to their measurements. Parser declarations describe possible output; telemetry records what was measured. Neither source names nor parser names establish a binding. A changed parser, missing source, or incomplete run produces work on the checklist rather than an invented observation.

Generation, checking and assessment reuse a matching confirmed inventory's active pack in Full and Quick modes, and can fill otherwise unspecified target context, reported as `CQL_QUICK_TARGET`. `--inventory-state FILE` selects a different protected state; an explicit `--pack` stays primary while matching inventory safeguards remain active. Identity checks happen before opening an inventory's active snapshot, separately for each JSONL row. A foreign inventory is skipped with a notice; a missing or corrupt matching snapshot blocks use even in ordinary mode. A missing default inventory leaves basic commands available and creates nothing.

Only Quick generation/checking can save accepted source/input-field requirements as unverified leads, reported as `CQL_QUICK_LEADS`. Full mode and assessment never capture leads. Assessment reuses pack rates without turning the query ledger into population evidence. An unverified lead is a suggestion, and an explicit saved-query observation remains caller-attested scoped evidence; neither silently alters the pack or marks Full discovery complete. Setup state does not select query identity. Use [query reference](QUERY-REFERENCE.md) to construct/check a query, then [result assessment](RESULT-ASSESSMENT.md) to inspect the saved execution result. Local readiness, measured grounding, and saved-result assessment are separate from vendor compilation, execution, and intent correctness. Deployment of rules, parsers, dashboards, or other Falcon configuration stays with the operator.
