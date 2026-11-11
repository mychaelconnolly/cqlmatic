# CQLmatic v0.1.5

CQLmatic keeps newer environment evidence active when you resume discovery and makes repeated imports easier to audit. Saved-query imports now identify query fields that still need directly returned values for strict checks.

## What changed

- Resuming a completed discovery preserves the active pack and retained history. First promotion verifies the relay's pack, receipt and predecessor. If another import changed the working inventory, prepare discovery again from the current pack.
- Repeated inventory actions preserve current-version state bytes when the action makes no change. Exact audit recovery, interrupted promotion and directory durability checks still run. Polling a failed discovery records its terminal failure once.
- Saved-query imports return `uncredited_fields` with guidance for fields absent from positive returned values. A filter field omitted by `select` appears in this feedback. Observation credit continues to require a directly returned value.
- Replaying a saved capture reuses its recorded observation and audit. Equivalent timestamp spellings represent the same instant. A different retrieval time for the same protected originals is a conflict. Separate captures with identical bytes can be retained using their own protected result files and distinct retrieval times.
- Scout imports validate the confirmed environment, current mode's repository and completed window. Replaying an older exact scout preserves the latest source counts. Every successful discovery capture, including Sources Only, must be retrieved at or after its probe window ends.
- Parser imports validate the retrieval timestamp before replacing the catalog, including requests with empty capture lists. Replaying an identical parser exclusion preserves reconciliation.
- Generation, checking and assessment name the actual Full or Quick inventory mode while retaining the informational `CQL_QUICK_TARGET` code.

## Compatibility

Existing inventory versions and saved observation IDs remain readable. A successful action migrates version-1 inventory as before. Record the actual UTC capture time when saving a result. Saved-query retrieval time is caller-attested.

MCP discovery state binds the executable version. Start a new bounded run with new state and receipt paths after upgrading. Retained environment packs remain available for query writing.

## Before using it

Basic query writing needs no setup or discovery. Start with [setup](docs/SETUP.md), then inspect [inventory and status](docs/INVENTORY.md) when environment evidence is needed. Authorize the target, window and finite budgets before any search. Full discovery can take substantial time and assistant tokens; parser inventory requires a separate Falcon API client with NGSIEM Parsers: READ.

The npm package targets macOS Apple silicon and Linux x86_64 without install scripts or additional binary downloads. Standalone packaging supports Linux x86_64 and Windows x64. Verify release archives against `SHA256SUMS` before extraction. Linux requires glibc 2.34 or newer and `libgcc_s.so.1`; the unsigned Windows executable uses a static CRT. Source builds require Rust 1.99.1.

`ready` describes local construction and the selected grounding policy. Neither it nor saved-result assessment proves vendor compilation, execution completeness or analyst-answer correctness. Saved-query observations retain the caller's assertion about query, target, window and response. Confirm operational results through your authorized CrowdStrike client; see [coverage and limits](docs/COVERAGE.md).

## Feedback

Include the version, platform and a minimal synthetic reproduction when reporting a problem. Keep credentials, tenant identifiers, packs and raw telemetry out of public issues. Use the [tester feedback form](https://github.com/mychaelconnolly/cqlmatic/issues/new/choose).
