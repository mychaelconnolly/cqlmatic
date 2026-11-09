# How CQLmatic works

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
ENVIRONMENT INVENTORY  <---------------------- status (offline checklist)
  |
  +-> CrowdStrike MCP scouting
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
                  RECONCILE + SAVE MAP
                  Observed / declared / historical / pending
                         |
                         v
USER REQUEST -> AI INTERPRETS -> CQLMATIC CONSTRUCTS + CHECKS -> CQL
                                  ^                            |
                                  | explicit environment pack  v
                                  +------------------- AUTHORIZED MCP SEARCH
                                                       |
                                                       v
                                             INSPECT + ASSESS SAVED RESULT
                                             refine, clarify, or hand off
```

The model reasons; the Rust engine applies typed contracts and deterministic rules. CQLmatic does not run its own LLM or replace your MCP client. The selected host owns OAuth, tool selection, and search execution. Codex, VS Code Local, Claude Code and Copilot surfaces have native setup adapters; other capable agents use tracked external setup. A saved host report is caller-supplied evidence, not independent authentication or configuration attestation. CQLmatic also supports explicit native Falcon API discovery for operators who use that path.

Follow [setup](SETUP.md), then [inventory](INVENTORY.md); use [contracts](CONTRACTS.md) for machine requests. `status --json` is the offline checklist, not a search or authentication refresh. `full_map_complete` is derived from the confirmed full target, applicable source/parser reviews and measurements, an active pack, and current reconciliation. Selected-source coverage remains useful without full-map completion.

State and packs are local snapshots, dated and scoped to their measurements. Parser declarations describe possible output; telemetry records what was measured. Neither source names nor parser names establish a binding. A changed parser, missing source, or incomplete run produces work on the checklist rather than an invented observation.

Generation always receives its pack explicitly. Setup state does not silently select a tenant or load an environment into a query. Use [query reference](QUERY-REFERENCE.md) to construct/check a query, then [result assessment](RESULT-ASSESSMENT.md) to inspect the saved execution result. Local readiness, measured grounding, and saved-result assessment are separate from vendor compilation, execution, and intent correctness. Deployment of rules, parsers, dashboards, or other Falcon configuration stays with the operator.
