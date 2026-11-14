# CQLmatic v0.1.6

CQLmatic helps you keep your environment map current and recover interrupted work. You can refresh manually, receive 30-day reminders, resume discovery, and give your assistant a small status response.

## What changed

- **Manual refresh and reminders.** Refresh the confirmed view's source list with a shallow census and a finite search lifetime. The inventory menu offers a reminder after 30 days, with a choice to refresh or be reminded in another 30 days. A reminder never starts a search. New source and field hints remain unverified work until inventory or an explicit exclusion resolves them.
- **Setup recovery.** An eligible, partially completed Codex CIMD setup can use an inspected pending state and a newly reviewed recovery preview to finish native login without replacing or duplicating its server entry. A matching saved host report is still required for authentication and tool discovery.
- **Resumable discovery.** New MCP checkpoints and native runs initialized with `--state` retain accepted captures across pause and resume. Retries use the remaining attempt and active-time budgets. Unknown search termination blocks further inventory searches until safely settled; stopping local waiting does not prove hosted cancellation.
- **Evidence fixes.** A sparse source with positive whole-window volume and an empty recent fieldset can receive one fallback fieldset over the original window. Measurements keep their actual source and window; field names alone do not establish populated-field evidence. Quoted Boolean-keyword field names round-trip consistently, and the supported `concat` signature uses a field list without a separator parameter.
- **Small status responses.** `status --json --brief` and inventory action brief output stay within 4,096 UTF-8 bytes. They expose readiness, counts, refresh reminders and bounded pending-work references while full and summary output remain available.

## Compatibility

Inventory state versions 1 and 2 remain readable. The next successful action writes version 3 atomically; earlier binaries cannot read newly written version-3 state, and reconciliation must be renewed after migration. Existing packs remain readable.

New discovery checkpoints use format 3. Compatible format-2 checkpoints keep their original absolute deadlines and failure behavior; they do not gain format-3 pause/resume allowances. Existing format-3 checkpoints keep their bound measurement policy. Completed format-2 checkpoints from `0.1.0` through `0.1.5` remain usable as inventory history when their target, captures and activation artifacts still validate. Executing or resuming discovery state remains bound to its executable version. Retained environment packs remain available for query writing. See [cancel and resume discovery](docs/ENVIRONMENT.md#cancel-and-resume-discovery) before upgrading an unfinished run.

## Before using it

Basic query writing needs no setup or discovery. Start with [setup](docs/SETUP.md), then inspect [inventory and status](docs/INVENTORY.md) when environment evidence is needed. Authorize the target, window and finite budgets before any search. Full discovery can take substantial time and assistant tokens; parser inventory requires a separate Falcon API client with NGSIEM Parsers: READ.

The npm package targets macOS Apple silicon and Linux x86_64 without install scripts or additional binary downloads. Standalone packaging supports Linux x86_64 and Windows x64. Verify release archives against `SHA256SUMS` before extraction. Linux requires glibc 2.34 or newer and `libgcc_s.so.1`; the unsigned Windows executable uses a static CRT. Source builds require Rust 1.99.1.

`ready` describes local construction and the selected grounding policy. Neither it nor saved-result assessment proves vendor compilation, execution completeness or analyst-answer correctness. Saved-query observations retain the caller's assertion about query, target, window and response. Confirm operational results through your authorized CrowdStrike client; see [coverage and limits](docs/COVERAGE.md).

## Feedback

Include the version, platform and a minimal synthetic reproduction when reporting a problem. Keep credentials, tenant identifiers, packs and raw telemetry out of public issues. Use the [tester feedback form](https://github.com/mychaelconnolly/cqlmatic/issues/new/choose).
