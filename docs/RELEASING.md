# Releasing CQLmatic

Release only with owner authorization for the intended actions. Prepare and validate privately before requesting public go-live approval. Approval identifies the exact source commit, annotated tag, package checksums and both website outputs. It does not authorize Falcon operations, real client configuration or paid services.

## Repository

Use the reviewed release-candidate checkout and preserve existing history, branches, tags and published assets. Check authenticated ownership, repository identity, visibility and refs before every push.

Use the owner's GitHub noreply identity for author, committer and annotated-tag metadata. The release tag names the final reviewed green commit.

Inspect the complete source, comments, reference data, workflows, metadata and history before uploading, including private GitHub uploads. Exclude credentials, tenant identifiers, raw telemetry, operational captures, local notes, caches and personal build paths. Preserve synthetic tests, necessary guides and attribution. Immediately before changing visibility, inspect all retained Actions logs, summaries, annotations and artifacts, including failed attempts.

## Freeze source and artifacts

1. Align Cargo/npm metadata, README, changelog and notes at `0.1.1`; keep schema and capability versions independent. Inspect and stage explicit reviewed paths. Generated binaries and website outputs stay outside the source allowlist.
2. Run the pinned formatting, Clippy, tests and locked builds in [development](DEVELOPMENT.md), all nine schemas, runnable examples and synthetic setup/inventory/MCP workflows without credentials. Reuse approved binaries only when all runtime-input digests remain identical; otherwise rebuild and review affected binaries.
3. Generate `SOURCE-SHA256SUMS` over every intended index path except itself. Require exact membership, regular files and index/worktree equality. Stage the checksum file, establish the final source commit and verify its tree against a separate full SHA-256 manifest outside the source tree.
4. Run Windows CI and generate both websites for that exact commit. Record successful run/artifact IDs and native digests outside tracked source. Source changes require new checksums, a new reviewed commit and affected checks/artifacts again. Do not put generated artifact hashes or the final commit ID into tracked source checksums.
5. Finish all archives, release notes, outer checksums and both website outputs before freezing review. Verify extracted exact membership and every file digest; reject missing, extra, stale, duplicate, linked or unsafe members and private metadata.
6. Obtain independent Sol max and Astra max reviews of identical frozen source, website and outbound manifests. Keep bytes unchanged during first passes. Resolve blockers, rerun affected checks and obtain targeted re-review. An unavailable required check/reviewer leaves the gate pending.
7. Create the annotated `v0.1.1` tag after these gates pass. Verify its annotation and peeled target against the reviewed commit. Record full commit/tag IDs and manifest hashes for approval. Never move the existing `v0.1.0` tag.

## Native packages

Build with Rust **1.99.1** and the lockfile. Remap source, dependency-cache and build paths to neutral prefixes, then inspect raw binaries and metadata before upload. Removing debug symbols alone does not prove private paths are absent.

Linux x86_64 requires glibc 2.34+ and `libgcc_s.so.1`. Windows CI builds `x86_64-pc-windows-msvc` with an explicit target and static CRT flags applied only to that target. It runs native checks and inspects ordinary/delayed PE imports before uploading `cqlmatic.exe` and `BUILD-WINDOWS.json`. Accept only Windows system DLL dependencies; distribute no PDBs and add no runtime compiler/SDK installer. Record the tested runner OS. Synthetic CI does not attest every filesystem, client or tenant configuration.

Use [the archive producer](../tools/package-release.py) with the frozen source manifest, approved Linux/Windows binaries, expected SHA-256 values and Windows build metadata. It requires a clean committed checkout, verifies index/tree/source-checksum membership, and checks archives after creation. Keep its build record outside distributed archives.

```sh
python3 tools/package-release.py build --manifest /path/to/reviewed-source.sha256 \
  --linux /path/to/cqlmatic-linux-x86_64 --linux-sha256 REVIEWED_LINUX_SHA256 \
  --windows /path/to/cqlmatic.exe --windows-sha256 REVIEWED_WINDOWS_SHA256 \
  --windows-metadata /path/to/BUILD-WINDOWS.json --out /path/to/packages
```

Release assets:

- `cqlmatic-v0.1.1-source.zip`
- `cqlmatic-v0.1.1-linux-x86_64.tar.gz`
- `cqlmatic-v0.1.1-windows-x86_64.zip`
- `cqlmatic-0.1.1.tgz`
- `SHA256SUMS`

Source/native archives use a `cqlmatic-v0.1.1/` root containing reviewed source and licenses. Linux adds `cqlmatic-linux-x86_64`; Windows adds `cqlmatic.exe`. Compare every extracted member with its reviewed source/native digest, rather than checking only the archive checksum.

## npm package

The launcher bundles `darwin-arm64` and `linux-x64` executables. It uses supported Node.js built-ins without runtime dependencies, install scripts or additional binary downloads. Use a [supported, patched Node.js release](../README.md#try-v010). Unsupported combinations fail rather than selecting/downloading another binary; Windows uses its ZIP.

Record approved native digests in `npm/native-manifest.json` before freezing source. From the frozen staged checkout:

```sh
python3 tools/package-npm.py --manifest /path/to/reviewed-source.sha256 \
  --linux /path/to/cqlmatic-linux-x86_64 \
  --macos /path/to/cqlmatic-macos-arm64 --out /path/to/packages
```

The producer checks source/checksum membership, selected npm members, native digests, extracted contents and archive metadata. The tarball includes `NPM-SHA256SUMS`. A source checkout contains no bundled executable, so ordinary `npm pack` there fails its prepack guard.

Test the actual tarball from a fresh directory/cache using isolated setup state and synthetic clients. Check argument/stdin forwarding, structured output, exit codes and interruption. Preserve documented npm stderr/process-supervision limits; do not configure a real client during tests.

Immediately before publishing, verify the signed-in npm owner, live package name and version availability. Availability is not a reservation. Stop for unexpected ownership or an existing version. Publish only the reviewed tarball with owner authentication/2FA, compare registry bytes with the reviewed digest and test the pinned command before activating public documentation that advertises it.

## Public GitHub release

After exact-byte go-live approval and npm verification, re-audit history and hosted CI output before changing visibility. Require the approved owner/name, reviewed remote `main`, no unexpected refs and intended new tag destination. Enable available secret scanning/push protection, dependency alerts/security updates and protection against force pushes/deletion of main/release refs. Actions default to read-only permissions.

Push only explicit reviewed objects to explicit destination refs after checking full IDs:

```sh
release_commit=$(git rev-parse refs/heads/main)
release_tag=$(git rev-parse refs/tags/v0.1.1)
git rev-parse 'refs/tags/v0.1.1^{commit}'
git status --porcelain
git remote -v
git ls-remote origin
git push --dry-run origin "$release_commit:refs/heads/main" "$release_tag:refs/tags/v0.1.1"
git push --atomic origin "$release_commit:refs/heads/main" "$release_tag:refs/tags/v0.1.1"
```

The peeled tag must equal the reviewed commit; the worktree must be clean and remote refs must match the inspected state. Never wildcard-push, force-push or move an existing release tag. Create the release with `--verify-tag`, reviewed notes through `--notes-file`, and explicitly selected assets/checksums. Verify public downloads, refs, registry integrity and CI. Correct a public executable with a new patch release rather than replacing immutable v0.1.1 assets.

## Project page and documentation

The project page is **https://cqlmatic.dev/** and complete documentation is **https://docs.cqlmatic.dev/**. Both use [Cloudflare Pages Direct Upload](https://developers.cloudflare.com/pages/get-started/direct-upload/) from one reviewed commit. Projects `cqlmatic` and `cqlmatic-docs` use production branch `main` without Git integration. Create them through the Pages API; do not use Wrangler's inferred creation or accept a Workers substitution. Use returned Pages hostnames instead of assuming globally available names.

Build and preview with the locked private site package:

```sh
npm --prefix site ci
npm --prefix site test
npm --prefix site run build -- --commit "$(git rev-parse HEAD)"
npm --prefix site run preview:landing
```

In a second terminal run `npm --prefix site run preview:docs`. These serve generated outputs locally without deploying or accessing remote bindings.

The immutable CI artifact contains `site/dist/landing`, `site/dist/docs`, `site/dist/BUILD.json` and `site/dist/SITE-SHA256SUMS`. The manifest covers both outputs and build metadata, excluding itself. These outputs stay out of tracked source. Each host serves its assets from the same origin, retains security headers and a true HTTP 404. Only landing redirects `/docs`, `/docs/` and `/docs/*` to corresponding documentation-host routes.

Automatic CI builds/checks both outputs without deployment secrets. Use [the manual workflow](../.github/workflows/deploy-pages.yml) with approved `source_sha`, successful main-push `ci_run_id`, `site_artifact_id` and reviewed `site_manifest_sha256`. It verifies repository/workflow/event/source provenance, safe exact membership and every digest. The protected job uploads checked bytes without rebuilding.

After the repository is public, configure the `production` environment with required owner approval, owner self-review permitted, administrative bypass disabled and main-only deployment. Verify protection before adding environment secrets `CLOUDFLARE_PAGES_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`. Keep the token account-scoped to Pages edit. Initial DNS setup uses separate owner authorization; do not widen routine token permissions. Pin tools/Actions and expose credentials only to the deployment controller step.

All production changes and restoration use fixed concurrency group `cqlmatic-production`, `cancel-in-progress: false`. Hold it from capturing previous deployment IDs through both uploads, domain activation, live checks and restoration. Deploy docs first, then landing, with explicit project, `--branch main` and approved `--commit-hash`. The pair is non-atomic. On failure, stop and restore changed projects to their preceding approved production deployments when available.

On first activation, verify both uploads on returned Pages hosts before associating `docs.cqlmatic.dev` and `cqlmatic.dev` through [Pages custom domains](https://developers.cloudflare.com/pages/configuration/custom-domains/). Perform owner-authorized domain/DNS setup while the workflow retains its concurrency lock. Check account/zone, existing records and conflicts; preserve unrelated settings and add no wildcard or `www` records. Wait for both DNS/certificate associations and live checks before declaring activation complete.

For intentional rollback, use the same protected workflow with an earlier approved commit, retained CI artifact and manifest digest. Retain approved bundles and project deployment IDs privately. If an artifact has expired or is unavailable, stop for reviewed recovery rather than presenting a rebuild as approved bytes. Do not perform an overlapping dashboard rollback outside the serialized workflow.

Verify each hostname's index/deep routes, assets, headers, HTTPS, canonical URLs, themes, keyboard access and mobile overflow. Check cross-host navigation, redirects/fragments and unknown routes returning HTTP 404. Compare deployed content with reviewed outputs and record exact source/version/deployment IDs. Keep credentials/account identifiers and private review records out of source and public logs.
