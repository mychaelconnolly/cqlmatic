#!/usr/bin/env python3
"""Credential-free regression checks for the Pages approval boundary."""

import contextlib
import copy
import hashlib
import importlib.util
import io
import json
import os
import stat
import tempfile
import unittest
import urllib.error
import warnings
import zipfile
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import patch


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


verify = load("verify-site")
deploy = load("deploy-pages")
SOURCE = "a" * 40
OLD_SOURCE = "b" * 40
MARKER = "CQLmatic Pages 123/1"
OLD_IDS = {"cqlmatic-docs": "11111111-1111-1111-1111-111111111111", "cqlmatic": "22222222-2222-2222-2222-222222222222"}
NEW_IDS = {"cqlmatic-docs": "33333333-3333-3333-3333-333333333333", "cqlmatic": "44444444-4444-4444-4444-444444444444"}


def site_files(source=SOURCE):
    files = {"landing/index.html": b"<h1>CQLmatic</h1>", "docs/index.html": b"<h1>Documentation</h1>",
             "landing/404.html": b"<h1>Not found</h1>", "docs/404.html": b"<h1>Not found</h1>",
             "landing/_headers": b"/*\n  X-Content-Type-Options: nosniff\n",
             "docs/setup/index.html": b"<h1>Setup</h1>",
             "BUILD.json": json.dumps({"source_commit": source, "version": "0.1.0", "documentation_pages": 29,
                                       "hosts": verify.HOSTS}, sort_keys=True).encode()}
    files["SITE-SHA256SUMS"] = "".join(hashlib.sha256(value).hexdigest() + "  " + name + "\n"
                                       for name, value in sorted(files.items())).encode()
    return files, hashlib.sha256(files["SITE-SHA256SUMS"]).hexdigest()


def archive(files, extra=()):
    result = io.BytesIO()
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", UserWarning)
        with zipfile.ZipFile(result, "w") as output:
            for name, value in files.items():
                output.writestr("site/dist/" + name, value)
            for name, value in extra:
                output.writestr(name, value)
    return result.getvalue()


def github_data():
    run = {"id": 10, "repository": {"full_name": verify.REPOSITORY, "id": 7},
           "head_repository": {"full_name": verify.REPOSITORY, "id": 7}, "workflow_id": 20,
           "path": ".github/workflows/ci.yml", "name": "CI", "event": "push", "head_branch": "main",
           "head_sha": SOURCE, "status": "completed", "conclusion": "success",
           "run_started_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:02:00Z"}
    artifact = {"id": 30, "name": "site-" + SOURCE, "expired": False, "digest": "sha256:" + "c" * 64,
                "created_at": "2026-01-01T00:01:00Z", "expires_at": "2026-02-01T00:00:00Z",
                "workflow_run": {"id": 10, "repository_id": 7, "head_repository_id": 7,
                                 "head_sha": SOURCE, "head_branch": "main"}}
    return {"/actions/runs/10": run, "/actions/workflows/ci.yml": {"id": 20, "path": ".github/workflows/ci.yml", "state": "active"},
            "/actions/artifacts/30": artifact, "/branches/main": {"commit": {"sha": SOURCE}},
            f"/compare/{SOURCE}...{SOURCE}": {"status": "identical", "merge_base_commit": {"sha": SOURCE}}}


class FakeGitHub:
    def __init__(self, data):
        self.data = data

    def get(self, path):
        return self.data[path]


class SiteArtifactTests(unittest.TestCase):
    def test_round_trip_preserves_every_file(self):
        files, digest = site_files()
        raw = archive(files)
        self.assertEqual(verify.read_archive(raw, SOURCE, digest), files)
        with tempfile.TemporaryDirectory() as work:
            out = Path(work) / "checked"
            verify.extract_archive(raw, out, SOURCE, digest)
            self.assertEqual(verify.check_directory(out / "site/dist", SOURCE, digest), files)

    def test_tamper_missing_extra_and_wrong_identity(self):
        files, digest = site_files()
        cases = []
        changed = dict(files)
        changed["landing/index.html"] += b"tampered"
        cases.append((changed, SOURCE, digest))
        changed = dict(files)
        del changed["docs/index.html"]
        cases.append((changed, SOURCE, digest))
        changed = dict(files, **{"docs/extra.html": b"extra"})
        cases.append((changed, SOURCE, digest))
        cases.extend(((files, OLD_SOURCE, digest), (files, "0" * 40, digest), (files, SOURCE, "0" * 64)))
        for values, source, approved in cases:
            with self.subTest(source=source, approved=approved, names=len(values)):
                with self.assertRaises(verify.InvalidSite):
                    verify.read_archive(archive(values), source, approved)

    def test_zip_rejects_traversal_links_duplicates_and_outside_files(self):
        files, digest = site_files()
        link = zipfile.ZipInfo("site/dist/docs/link")
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        cases = [("site/dist/docs/../escape", b"x"), ("outside.txt", b"x"),
                 ("site/dist/landing/index.html", b"duplicate"), ("site/dist/LANDING/INDEX.HTML", b"duplicate"),
                 ("site/dist/docs/back\\slash", b"x"), ("site/dist/docs/%2e%2e", b"x"),
                 ("site/dist/docs/.env", b"x"), ("site/dist/docs/_worker.js", b"x"),
                 ("site/dist/docs/functions/index.js", b"x"), ("site/dist/BUILD.json/child", b"x"),
                 ("site/dist/docs/hidden-payload/", b"x"), (link, b"target")]
        for extra in cases:
            with self.subTest(name=str(extra[0])):
                with self.assertRaises(verify.InvalidSite):
                    verify.read_archive(archive(files, [extra]), SOURCE, digest)

    def test_manifest_duplicates_unsorted_and_missing_digest_are_rejected(self):
        files, _ = site_files()
        lines = files["SITE-SHA256SUMS"].decode().splitlines(keepends=True)
        for manifest in ("".join(reversed(lines)), "".join(lines + lines[:1]), "".join(lines[1:])):
            changed = dict(files)
            changed["SITE-SHA256SUMS"] = manifest.encode()
            with self.assertRaises(verify.InvalidSite):
                verify.read_archive(archive(changed), SOURCE, hashlib.sha256(manifest.encode()).hexdigest())

    def test_encrypted_null_byte_and_double_slash_members_are_rejected(self):
        files, digest = site_files()
        raw = bytearray(archive(files))
        for signature, flag_offset in ((b"PK\x03\x04", 6), (b"PK\x01\x02", 8)):
            index = raw.find(signature)
            raw[index + flag_offset] |= 1
        with self.assertRaises(verify.InvalidSite):
            verify.read_archive(bytes(raw), SOURCE, digest)
        null_path = archive(files, [("site/dist/docs/Aevil", b"x")]).replace(b"docs/Aevil", b"docs/\x00evil")
        with self.assertRaises(verify.InvalidSite):
            verify.read_archive(null_path, SOURCE, digest)
        with self.assertRaises(verify.InvalidSite):
            verify.read_archive(archive(files, [("site/dist/docs/double//", b"")]), SOURCE, digest)

    def test_file_directory_collision_and_size_budgets_are_rejected(self):
        files, digest = site_files()
        with self.assertRaises(verify.InvalidSite):
            verify.read_archive(archive(files, [("site/dist/docs/setup", b"file")]), SOURCE, digest)
        with patch.object(verify, "MAX_EXPANDED", 1), self.assertRaises(verify.InvalidSite):
            verify.read_archive(archive(files), SOURCE, digest)

    def test_zero_build_commit_and_wrong_hosts_are_rejected(self):
        for field, value in (("source_commit", "0" * 40), ("hosts", {"landing": "https://example.com"}),
                             ("version", "0.2.0"), ("documentation_pages", 28), ("unexpected_metadata", "synthetic")):
            files, _ = site_files()
            build = json.loads(files["BUILD.json"])
            build[field] = value
            files["BUILD.json"] = json.dumps(build).encode()
            del files["SITE-SHA256SUMS"]
            files["SITE-SHA256SUMS"] = "".join(hashlib.sha256(raw).hexdigest() + "  " + name + "\n"
                                               for name, raw in sorted(files.items())).encode()
            with self.subTest(field=field), self.assertRaises(verify.InvalidSite):
                verify.read_archive(archive(files), SOURCE, hashlib.sha256(files["SITE-SHA256SUMS"]).hexdigest())

    def test_extracted_symlink_is_rejected(self):
        files, digest = site_files()
        with tempfile.TemporaryDirectory() as work:
            out = Path(work) / "checked"
            verify.extract_archive(archive(files), out, SOURCE, digest)
            target = out / "site/dist/docs/link"
            target.symlink_to(out / "site/dist/landing/index.html")
            with self.assertRaises(verify.InvalidSite):
                verify.check_directory(out / "site/dist", SOURCE, digest)


class ProvenanceTests(unittest.TestCase):
    def test_successful_exact_main_ci_run_is_accepted(self):
        data = github_data()
        self.assertEqual(verify.verify_provenance(FakeGitHub(data), SOURCE, 10, 30,
                         datetime(2026, 1, 2, tzinfo=timezone.utc))["id"], 30)

    def test_wrong_branch_event_run_workflow_artifact_and_reachability_are_rejected(self):
        run_key, artifact_key = "/actions/runs/10", "/actions/artifacts/30"
        cases = [(run_key, "head_branch", "feature"), (run_key, "conclusion", "failure"),
                 (run_key, "status", "in_progress"), (run_key, "event", "workflow_dispatch"),
                 (run_key, "head_sha", OLD_SOURCE), (run_key, "workflow_id", 21),
                 (run_key, "path", ".github/workflows/other.yml"), (artifact_key, "id", 31),
                 (artifact_key, "name", "site-other"), (artifact_key, "expired", True),
                 (artifact_key, "created_at", "2025-12-31T23:59:00Z"),
                 (artifact_key, "digest", None), (artifact_key, "expires_at", "2026-01-01T00:00:00Z")]
        for key, field, value in cases:
            data = github_data()
            data[key][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(verify.InvalidSite):
                verify.verify_provenance(FakeGitHub(data), SOURCE, 10, 30, datetime(2026, 1, 2, tzinfo=timezone.utc))
        for field, value in (("id", 11), ("head_sha", OLD_SOURCE), ("repository_id", 8)):
            data = github_data()
            data[artifact_key]["workflow_run"][field] = value
            with self.subTest(binding=field), self.assertRaises(verify.InvalidSite):
                verify.verify_provenance(FakeGitHub(data), SOURCE, 10, 30, datetime(2026, 1, 2, tzinfo=timezone.utc))
        data = github_data()
        data[f"/compare/{SOURCE}...{SOURCE}"]["status"] = "diverged"
        with self.assertRaises(verify.InvalidSite):
            verify.verify_provenance(FakeGitHub(data), SOURCE, 10, 30, datetime(2026, 1, 2, tzinfo=timezone.utc))

    def test_download_redirect_never_forwards_github_token_to_storage(self):
        storage = "https://productionresultssa1.blob.core.windows.net/actions-results/site.zip?signature=value"
        redirect = urllib.error.HTTPError("https://api.github.com/", 302, "redirect", {"Location": storage}, None)
        with patch.object(verify, "read_url", side_effect=(redirect, b"zip")) as read:
            self.assertEqual(verify.GitHub("synthetic-token").download(30), b"zip")
            self.assertIn("Authorization", read.call_args_list[0].args[1])
            self.assertEqual(read.call_args_list[1].args[1], {})
        redirect = urllib.error.HTTPError("https://api.github.com/", 302, "redirect", {"Location": "https://example.com/site.zip"}, None)
        with patch.object(verify, "read_url", side_effect=redirect), self.assertRaises(verify.InvalidSite):
            verify.GitHub("synthetic-token").download(30)

    def test_prepare_preserves_original_archive_bytes_and_publishes_only_safe_proof(self):
        files, digest = site_files()
        raw, data = archive(files), github_data()
        data["/actions/artifacts/30"].update({"digest": "sha256:" + hashlib.sha256(raw).hexdigest(),
                                            "expires_at": "2100-01-01T00:00:00Z"})
        api = FakeGitHub(data)
        api.download = lambda artifact_id: raw
        with tempfile.TemporaryDirectory() as work:
            out = Path(work) / "transfer"
            argv = ["verify-site.py", "prepare", "--source-sha", SOURCE, "--manifest-sha256", digest,
                    "--run-id", "10", "--artifact-id", "30", "--out", str(out)]
            with patch.object(verify, "GitHub", return_value=api), patch.object(verify.sys, "argv", argv), contextlib.redirect_stdout(io.StringIO()):
                verify.main()
            self.assertEqual((out / "site.zip").read_bytes(), raw)
            proof = json.loads((out / "provenance.json").read_bytes())
            self.assertEqual(set(proof), {"repository", "source_commit", "ci_run_id", "site_artifact_id",
                                         "site_manifest_sha256", "archive_sha256"})
            self.assertEqual(proof["site_manifest_sha256"], digest)


class FakeCloudflare:
    def __init__(self, first_launch=False):
        self.calls = []
        self.projects = {name: {"id": "project-" + name, "name": name, "source": None,
                               "production_branch": "main", "uses_functions": False, "subdomain": name + ".pages.dev"}
                         for name, _, _ in deploy.PROJECTS}
        self.current = {name: None if first_launch else OLD_IDS[name] for name in self.projects}
        self.records = {}
        for name in self.projects:
            self.records[OLD_IDS[name]] = self.record(name, OLD_IDS[name], OLD_SOURCE, "previous")

    def record(self, name, deployment_id, source=SOURCE, marker=MARKER):
        return {"id": deployment_id, "project_name": name, "project_id": "project-" + name,
                "environment": "production", "uses_functions": False,
                "deployment_trigger": {"type": "ad_hoc", "metadata": {"branch": "main", "commit_hash": source, "commit_message": marker}},
                "latest_stage": {"name": "deploy", "status": "success"},
                "url": "https://" + deployment_id[:8] + "." + name + ".pages.dev"}

    def project(self, name):
        self.calls.append(("get-project", name))
        value = copy.deepcopy(self.projects[name])
        value["canonical_deployment"] = {"id": self.current[name]} if self.current[name] else None
        return value

    def deployment(self, name, deployment_id):
        self.calls.append(("get-deployment", name, deployment_id))
        return copy.deepcopy(self.records[deployment_id])

    def deployments(self, name):
        return [copy.deepcopy(value) for value in self.records.values() if value["project_name"] == name]

    def rollback(self, name, deployment_id):
        self.calls.append(("rollback", name, deployment_id))
        self.current[name] = deployment_id
        return self.deployment(name, deployment_id)

    def domain(self, name, domain):
        self.calls.append(("get-domain", name, domain))
        return {"name": domain, "status": "active", "validation_data": {"status": "active"},
                "verification_data": {"status": "active"}}

    def upload(self, wrangler, directory, name, source, marker):
        self.calls.append(("upload", name))
        deployment_id = NEW_IDS[name]
        self.records[deployment_id] = self.record(name, deployment_id, source, marker)
        self.current[name] = deployment_id
        return deployment_id, True


class DeploymentTests(unittest.TestCase):
    def run_deploy(self, client, upload=None, check=None):
        files, digest = site_files()
        with contextlib.redirect_stdout(io.StringIO()):
            return deploy.deploy_sites(client, Path("dist"), files, SOURCE, digest, Path("wrangler"), MARKER, 0,
                                       upload_site=upload or client.upload, check_content=check or (lambda *args: None))

    def test_docs_first_and_both_immutable_and_custom_hosts_checked(self):
        client, checked = FakeCloudflare(), []
        result = self.run_deploy(client, check=lambda *args: checked.append(args[0]))
        self.assertEqual(result["status"], "verified")
        self.assertEqual([call for call in client.calls if call[0] == "upload"], [("upload", "cqlmatic-docs"), ("upload", "cqlmatic")])
        self.assertEqual(checked, ["https://33333333.cqlmatic-docs.pages.dev", "https://44444444.cqlmatic.pages.dev",
                                   "https://docs.cqlmatic.dev", "https://cqlmatic.dev"])
        self.assertFalse(any(call[0] == "rollback" for call in client.calls))

    def test_partial_upload_failure_restores_exact_previous_docs_id(self):
        client = FakeCloudflare()
        def upload(*args):
            return (None, False) if args[2] == "cqlmatic" else client.upload(*args)
        result = self.run_deploy(client, upload=upload)
        self.assertEqual(result["status"], "failed")
        self.assertEqual(client.current, OLD_IDS)
        self.assertEqual([call for call in client.calls if call[0] == "rollback"], [("rollback", "cqlmatic-docs", OLD_IDS["cqlmatic-docs"])])

    def test_failed_verification_restores_both_in_reverse_order(self):
        client = FakeCloudflare()
        def check(host, *args):
            if host.endswith(".cqlmatic.pages.dev"):
                raise deploy.InvalidSite("Synthetic content mismatch.")
        result = self.run_deploy(client, check=check)
        self.assertEqual(result["status"], "failed")
        self.assertEqual(client.current, OLD_IDS)
        self.assertEqual([call for call in client.calls if call[0] == "rollback"],
                         [("rollback", "cqlmatic", OLD_IDS["cqlmatic"]), ("rollback", "cqlmatic-docs", OLD_IDS["cqlmatic-docs"])])

    def test_uncertain_cli_failure_binds_deployment_before_restoration(self):
        client = FakeCloudflare()
        def upload(*args):
            value = client.upload(*args)
            return (None, False) if args[2] == "cqlmatic" else value
        result = self.run_deploy(client, upload=upload)
        self.assertEqual(result["status"], "failed")
        self.assertEqual(client.current, OLD_IDS)
        self.assertEqual(result["deployments"], NEW_IDS)

    def test_newer_unrelated_production_is_never_rolled_back(self):
        client = FakeCloudflare()
        newer_id = "55555555-5555-5555-5555-555555555555"
        def check(host, *args):
            if host.endswith(".cqlmatic.pages.dev"):
                client.records[newer_id] = client.record("cqlmatic-docs", newer_id, OLD_SOURCE, "another-run")
                client.current["cqlmatic-docs"] = newer_id
                raise deploy.InvalidSite("Synthetic content mismatch.")
        result = self.run_deploy(client, check=check)
        self.assertEqual(result["restoration"]["cqlmatic-docs"], "blocked-newer-production")
        self.assertEqual(client.current["cqlmatic-docs"], newer_id)
        self.assertFalse(any(call[:2] == ("rollback", "cqlmatic-docs") for call in client.calls))

    def test_docs_drift_during_final_domain_check_fails_and_restores_owned_landing(self):
        client = FakeCloudflare()
        newer_id = "55555555-5555-5555-5555-555555555555"
        def check(host, *args):
            if host == "https://cqlmatic.dev":
                client.records[newer_id] = client.record("cqlmatic-docs", newer_id, OLD_SOURCE, "another-run")
                client.current["cqlmatic-docs"] = newer_id
        result = self.run_deploy(client, check=check)
        self.assertEqual(result["status"], "failed")
        self.assertEqual(client.current["cqlmatic-docs"], newer_id)
        self.assertEqual(client.current["cqlmatic"], OLD_IDS["cqlmatic"])
        self.assertEqual(result["restoration"]["cqlmatic-docs"], "blocked-newer-production")
        self.assertEqual([call for call in client.calls if call[0] == "rollback"],
                         [("rollback", "cqlmatic", OLD_IDS["cqlmatic"])])

    def test_first_launch_failure_never_creates_projects_or_domains(self):
        client = FakeCloudflare(first_launch=True)
        def check(*args):
            raise deploy.InvalidSite("Synthetic content mismatch.")
        result = self.run_deploy(client, check=check)
        self.assertEqual(result["restoration"]["cqlmatic-docs"], "unavailable-first-deployment")
        self.assertFalse(any(call[0] in ("rollback", "get-domain") for call in client.calls))

    def test_domain_timeout_restores_both_projects(self):
        client = FakeCloudflare()
        client.domain = lambda *args: None
        result = self.run_deploy(client)
        self.assertEqual(result["status"], "failed")
        self.assertEqual(client.current, OLD_IDS)
        self.assertEqual(set(result["restoration"]), set(OLD_IDS))

    def test_returned_pages_subdomain_is_used_when_project_name_is_taken(self):
        client, checked = FakeCloudflare(), []
        client.projects["cqlmatic"]["subdomain"] = "cqlmatic-other.pages.dev"
        def upload(*args):
            result = client.upload(*args)
            if args[2] == "cqlmatic":
                client.records[result[0]]["url"] = "https://44444444.cqlmatic-other.pages.dev"
            return result
        result = self.run_deploy(client, upload=upload, check=lambda *args: checked.append(args[0]))
        self.assertEqual(result["status"], "verified")
        self.assertIn("https://44444444.cqlmatic-other.pages.dev", checked)

    def test_missing_or_integrated_projects_block_all_uploads(self):
        for change in ({"source": {"type": "github"}}, {"production_branch": "production"}, {"uses_functions": True}):
            client = FakeCloudflare()
            client.projects["cqlmatic"].update(change)
            result = self.run_deploy(client)
            self.assertEqual(result["status"], "failed")
            self.assertFalse(any(call[0] == "upload" for call in client.calls))

    def test_cf_controller_rejects_unscoped_targets_before_network(self):
        client = deploy.Cloudflare("a" * 32, "synthetic-token")
        with self.assertRaises(deploy.InvalidSite):
            client.request("another-project")
        with self.assertRaises(deploy.InvalidSite):
            client.request("cqlmatic", method="POST")

    def test_clean_html_and_404_routes(self):
        self.assertEqual(deploy.served_path("index.html"), "/")
        self.assertEqual(deploy.served_path("setup/index.html"), "/setup/")
        self.assertEqual(deploy.served_path("guide.html"), "/guide")
        self.assertEqual(deploy.served_path("assets/style.css"), "/assets/style.css")
        self.assertEqual(deploy.served_path("404.html"), "/__cqlmatic_verify_missing_page__")

    def test_https_checks_hashes_clean_routes_and_the_real_404_response(self):
        files, _ = site_files()
        seen = []
        class Response(io.BytesIO):
            status = 200
        class Opener:
            def open(self, request, **kwargs):
                seen.append(request.full_url)
                path = deploy.urllib.parse.urlsplit(request.full_url).path
                key = {"/": "docs/index.html", "/setup/": "docs/setup/index.html",
                       "/__cqlmatic_verify_missing_page__": "docs/404.html"}[path]
                if path == "/__cqlmatic_verify_missing_page__":
                    raise urllib.error.HTTPError(request.full_url, 404, "missing", {}, io.BytesIO(files[key]))
                return Response(files[key])
        with patch.object(deploy.urllib.request, "build_opener", return_value=Opener()):
            deploy.verify_content("https://docs.cqlmatic.dev", files, "docs", SOURCE)
        self.assertEqual(len(seen), 3)
        self.assertTrue(any("/setup/?" in url for url in seen))
        class ChangedOpener:
            def open(self, request, **kwargs):
                return Response(b"changed")
        with patch.object(deploy.urllib.request, "build_opener", return_value=ChangedOpener()), self.assertRaises(deploy.InvalidSite):
            deploy.verify_content("https://docs.cqlmatic.dev", files, "docs", SOURCE)

    def test_wrangler_credentials_only_env_and_no_repository_config(self):
        with tempfile.TemporaryDirectory() as work:
            directory = Path(work) / "site"
            directory.mkdir()
            (directory / "index.html").write_text("content", encoding="utf-8")
            def run(command, **kwargs):
                env = kwargs["env"]
                self.assertNotIn("synthetic-token", command)
                self.assertNotIn("GITHUB_TOKEN", env)
                self.assertNotEqual(kwargs["cwd"], directory)
                self.assertEqual(sorted(path.name for path in kwargs["cwd"].iterdir()), ["assets"])
                Path(env["WRANGLER_OUTPUT_FILE_PATH"]).write_text(json.dumps({"type": "pages-deploy", "pages_project": "cqlmatic",
                                                                           "deployment_id": NEW_IDS["cqlmatic"]}) + "\n", encoding="utf-8")
                return deploy.subprocess.CompletedProcess(command, 0)
            with patch.dict(os.environ, {"CLOUDFLARE_API_TOKEN": "synthetic-token", "CLOUDFLARE_ACCOUNT_ID": "a" * 32,
                                         "GITHUB_TOKEN": "synthetic-github-token"}), patch.object(deploy.subprocess, "run", side_effect=run):
                self.assertEqual(deploy.upload(Path("wrangler"), directory, "cqlmatic", SOURCE, MARKER), (NEW_IDS["cqlmatic"], True))

if __name__ == "__main__":
    unittest.main()
