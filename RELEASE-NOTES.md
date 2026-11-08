# CQLmatic v0.1.0

CQLmatic v0.1.0 is the first public release. Testers and feedback are welcome.

CQLmatic helps your AI assistant map your CrowdStrike Next-Gen SIEM environment and write CQL tailored to your sources, fields, and parser behavior. The assistant interprets your request; the local Rust engine constructs and checks the query against explicit contracts and saved environment evidence.

## In this release

- Guided native-client setup for Codex CLI and VS Code, with reviewed configuration commands and separate authentication and tool-discovery steps.
- Resumable environment inventory, parser review, measured source/field discovery, and an offline status checklist.
- Local query generation from typed plans, recipes, and bounded English, plus checking, linting, reference lookup, and saved-result assessment.
- A compact CQLmatic wordmark in interactive setup. Structured and redirected output retain their existing formats.
- On-demand setup through `npx cqlmatic@0.1.0 setup` on macOS Apple silicon and Linux x86_64, using a supported, patched Node.js release as specified in the [README](README.md#try-v010).
- Apache-2.0 licensing, synthetic examples, and setup, inventory, and companion-workflow guides.

## Before you try it

Start with the [project page](https://cqlmatic.dev/), [documentation](https://docs.cqlmatic.dev/) and [setup guide](https://docs.cqlmatic.dev/setup/). Each installation starts with an empty environment reference. Initial discovery can take substantial time and assistant tokens; review the target and limits first. Full parser inventory needs a separate Falcon API client with NGSIEM Parsers: READ.

The npm package bundles macOS Apple silicon and Linux x86_64 executables without install scripts or additional binary downloads. Standalone packages are available for Linux x86_64 and Windows x64. Verify downloads against `SHA256SUMS` before extraction. Linux requires glibc 2.34 or newer and `libgcc_s.so.1`. The Windows ZIP contains an unsigned, statically linked executable that needs no Rust or C++ build tools. Source builds require Rust 1.99.1.

A `ready` result describes local construction and grounding checks. Confirm operational results through your authorized CrowdStrike client. See [coverage and limits](docs/COVERAGE.md).

## Feedback

Report where setup or mapping got stuck, the intended query, and how the result differed from expectations. Include the version, platform, and a minimal synthetic reproduction. Keep credentials, tenant identifiers, and raw telemetry out of public issues.

Use the tester-feedback form in the [project's Issues](https://github.com/mychaelconnolly/cqlmatic/issues/new/choose).
