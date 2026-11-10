# CQLmatic documentation

CQLmatic maps a CrowdStrike NG-SIEM environment into a reusable local snapshot and constructs/checks CQL against explicit intent and evidence. The host analyst or model reasons about the question and parser meaning; the Rust CLI supplies deterministic mechanics. It makes no internal model calls.

Read these guides at [docs.cqlmatic.dev](https://docs.cqlmatic.dev/), visit the [project page](https://cqlmatic.dev/), or use the Markdown files offline with a release archive.

This manual describes **CQLmatic 0.1.2**, JSON contracts **v1**, and executable profile **`cqlmatic-ngsiem-core-v2`**. Use `cqlmatic catalog --profile` for the running binary's knowledge revision and limits. These identifiers do not attest to a tenant, execution, completeness, or unrestricted-English accuracy.

## Start as an operator

1. Read [the project README](../README.md) for prerequisites and executable/source-build instructions, then [setup and authentication](SETUP.md).
2. Follow [inventory and status](INVENTORY.md): scout visible sources, confirm the target/context, inventory and review parsers, measure fields, and reconcile evidence. Review costs and budgets first.
3. Read [environment packs](ENVIRONMENT.md) to inspect/import/refresh evidence and understand its limits.
4. Use [query construction](QUERY-REFERENCE.md), [the command reference](../USAGE.md), and [result assessment](RESULT-ASSESSMENT.md) for the question at hand.
5. Use [troubleshooting](TROUBLESHOOTING.md) to decide whether to fix input, resume, or start a new bounded run.

## Start as an agent integrator

1. Read [application flow](FLOW.md), [machine contracts](CONTRACTS.md), and [CLI modes/exits](../USAGE.md).
2. Follow [the analyst workflow](ANALYST-WORKFLOW.md) and [MCP companion loop](MCP-COMPANION.md). The optional [agent snippet](../integrations/agent/AGENTS-snippet.md) and [agent skill](../integrations/agent/SKILL.md) point to the same workflow.
3. Retrieve the relevant status/pack and catalog entries explicitly; setup/discovery does not create model memory or silently select the generation pack.
4. Construct/review typed requests, execute only through an authorized client with the confirmed target/window, retain exact query/result bytes, and assess/report limits.

## Guides and references

| Document | Use it for |
| --- | --- |
| [Setup](SETUP.md) | Native Codex, VS Code Local, Claude Code and Copilot adapters; guided external hosts, OAuth identity, saved evidence and recovery. |
| [Inventory](INVENTORY.md) | Every guided action, parser review/bindings, field batches, completion and status. |
| [Application flow](FLOW.md) | End-to-end operator/model/CLI boundaries. |
| [Command reference](../USAGE.md) | Every command/flag, default, mode, input/output and exit policy. |
| [Contracts](CONTRACTS.md) | Nine schemas, request/result/state fields, JSONL and diagnostics. |
| [Query reference](QUERY-REFERENCE.md) | Typed plans, all recipes/functions, bounded English and raw CQL checks. |
| [Environment](ENVIRONMENT.md) | Identity, credentials, native/legacy packs, discovery budgets, evidence and refresh. |
| [MCP companion](MCP-COMPANION.md) | Exact agent-relayed probes/results, state lifecycle, activation and recovery. |
| [Result assessment](RESULT-ASSESSMENT.md) | Saved-result checks, caps, nulls, empty/zero, outcomes and limits. |
| [Coverage](COVERAGE.md) | Implemented profile, assurance boundaries and exclusions. |
| [Troubleshooting](TROUBLESHOOTING.md) | Diagnostic families and safe next actions. |
| [Architecture](ARCHITECTURE.md) | Data flow, module ownership, I/O and trust boundaries. |
| [Development](DEVELOPMENT.md), [contributing](../CONTRIBUTING.md), [releasing](RELEASING.md) | Build, validation, contribution and authorized publication procedures. |

## Offline, authenticated, and local effects

| Operation | Network/authentication | Local effects |
| --- | --- | --- |
| Generate/check, catalog, schemas, assess, inspect, status | Offline; no Falcon/model key needed. | Read explicit inputs/state; emit assessments. |
| No-request `setup --json` | Offline; no native capability/config probing. | Read saved setup status without creating state. |
| Setup detect/select/preview/apply/import | Native capability probes are local; Codex apply can open browser consent. Copilot file writes and Claude add do not authenticate. External selection and report import do not connect to Falcon. | Save setup state and caller-supplied report hash; preview/backup and reviewed local configuration for named hosts. |
| Inventory actions | Most are local; parser fetch uses a separate protected Falcon API client; emitted scout/discovery probes are run by the host's authorized MCP connection. | Save sensitive workflow evidence, snapshots, parser review and cumulative active pack. |
| `env identity` | Reads credentials, no network. | Emit local account binding; not a provider-verified CID. |
| `env import` | Offline. | Validate and create/explicitly replace a protected pack. |
| `env discover` without execution/relay flags | Offline plan. | Print initial probes/deferred stages; no credentials or searches. |
| `env discover --mcp` | CLI relays saved results; host performs authorized read-only searches. | Save state/captures and eventually activate pack/receipt. |
| `env discover --execute`, `env parsers` | Explicit authenticated Falcon API operation. | Bounded jobs/cleanup or metadata reads; discovery saves report/pack. |

The API search-job lifecycle needs READ and WRITE scopes for creating/polling/stopping jobs; it does not write tenant configuration. Read-only MCP search and parser API reads have separate permissions. Client OAuth consent is not proof of telemetry visibility or parser access. See [permissions](SETUP.md).

## Runnable synthetic material

- [Setup requests](../examples/setup/README.md).
- [Inventory and parser review](../examples/inventory/README.md).
- [MCP relay, generation and assessment](../examples/mcp/README.md).
- [Query-family requests and expected CQL](../examples/query-reference/README.md).
- [Offline observation import](ENVIRONMENT.md#offline-observation-example).
- [Assessment outcomes](RESULT-ASSESSMENT.md#offline-outcome-examples).

Examples demonstrate mechanics, not real source names, observed tenant fields, or runtime correctness. Use protected operator-supplied identity/data for authorized operations. No automatic query execution, tenant-configuration deployment, broad search authorization, model memory, or evidence freshness policy is implied.

For sensitive issues follow [security reporting](../SECURITY.md). Keep credentials, environment maps, parser definitions, states, receipts, and raw results out of public feedback. Licensing and retained third-party attribution are in [LICENSE](../LICENSE) and [NOTICE](../NOTICE).
