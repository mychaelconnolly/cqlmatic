# CQLmatic v0.1.3

## Summary

Quick inventory lets you start with a few chosen data sources before mapping your whole environment. You can write basic queries immediately, save source and field names as leads, and switch to the full workflow later without losing what you found. If you save a query and its complete result, you can import them to record which fields had values. CQLmatic cannot independently verify that the result came from that query.

## Technical details

- Quick inventory scopes an authorized scout to selected sources, with a chosen view or `search-all`, explicit UTC window, and finite query and time budgets. Settings can return to Full without discarding saved evidence.
- Basic generation and checking work without a pack. With a confirmed Quick target, they can save source and input-field names as unverified leads; they do not store query text or literal values in inventory state.
- An explicit import can record caller-attested field observations from an exact saved query and complete response. Only directly returned nonempty values from one source qualify. Strict grounding respects later zero or stale measurements and active-pack record-kind ambiguity.
- Type-only sources and compatible repeated historical pack imports are supported. Imported sources remain unconfirmed until explicitly selected.
- Opt-in JSON summaries reduce initial context for inventory, status and pack inspection. Strict partial coverage requires analyst acceptance; unscoped raw-text regex or wildcard queries receive a cost warning.
- Setup reliability and diagnostic improvements from v0.1.2 remain included.
- On-demand setup through the `0.1.3` native executable on macOS Apple silicon, Linux x86_64 and Windows x64. The npm launcher and Windows ZIP are produced from separately reviewed native builds.
- Apache-2.0 licensing, synthetic examples, and setup, inventory, and companion-workflow guides.

## Before you try it

Start with the [project page](https://cqlmatic.dev/), [documentation](https://docs.cqlmatic.dev/) and [setup guide](https://docs.cqlmatic.dev/setup/). Each installation starts with an empty environment reference. Basic query writing needs no discovery. Review the target and limits before any authorized scout; Full discovery can take substantial time and assistant tokens. Full parser inventory needs a separate Falcon API client with NGSIEM Parsers: READ.

The npm package bundles macOS Apple silicon and Linux x86_64 executables without install scripts or additional binary downloads. Standalone packages are available for Linux x86_64 and Windows x64. Verify downloads against `SHA256SUMS` before extraction. Linux requires glibc 2.34 or newer and `libgcc_s.so.1`. The Windows ZIP contains an unsigned, statically linked executable that needs no Rust or C++ build tools. Source builds require Rust 1.99.1.

A `ready` result describes local construction and grounding checks. Saved-query observations depend on the caller's truthful association of query, target, window and response; CQLmatic does not independently attest execution. Older CLIs cannot read a migrated Quick inventory state. Confirm operational results through your authorized CrowdStrike client. See [coverage and limits](docs/COVERAGE.md).

## Feedback

Report where setup or mapping got stuck, the intended query, and how the result differed from expectations. Include the version, platform, and a minimal synthetic reproduction. Keep credentials, tenant identifiers, and raw telemetry out of public issues.

Use the tester-feedback form in the [project's Issues](https://github.com/mychaelconnolly/cqlmatic/issues/new/choose).
