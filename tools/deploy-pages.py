#!/usr/bin/env python3
"""Upload one approved artifact to existing Pages projects and verify production."""

import argparse
import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

spec = importlib.util.spec_from_file_location("site_verifier", Path(__file__).with_name("verify-site.py"))
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)
InvalidSite, require = verifier.InvalidSite, verifier.require
PROJECTS = (("cqlmatic-docs", "docs", "docs.cqlmatic.dev"), ("cqlmatic", "landing", "cqlmatic.dev"))
DEPLOYMENT_ID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\Z")


class Cloudflare:
    def __init__(self, account_id, token):
        require(isinstance(account_id, str) and re.fullmatch(r"[0-9a-f]{32}", account_id) and bool(token),
                "Scoped Cloudflare Pages credentials are required.")
        self.base = "https://api.cloudflare.com/client/v4/accounts/" + account_id + "/pages/projects/"
        self.token = token

    def request(self, project, suffix="", method="GET", missing_ok=False):
        require(project in {item[0] for item in PROJECTS} and
                (suffix == "" or re.fullmatch(r"/deployments(?:\?per_page=100|/[0-9a-f-]{36}(?:/rollback)?)?", suffix) or
                 suffix in ("/domains/cqlmatic.dev", "/domains/docs.cqlmatic.dev")), "An API target is outside the Pages scope.")
        require(method == "GET" or (method == "POST" and suffix.endswith("/rollback")),
                "Only uploads and restoration of approved Pages deployments are supported.")
        request = urllib.request.Request(self.base + project + suffix, data=b"{}" if method == "POST" else None,
                                         method=method, headers={"Authorization": "Bearer " + self.token,
                                         "Content-Type": "application/json", "User-Agent": "cqlmatic-pages-deploy"})
        try:
            with urllib.request.build_opener(verifier.NoRedirect()).open(request, timeout=30) as response:
                raw = response.read(2 * 1024 * 1024 + 1)
                require(len(raw) <= 2 * 1024 * 1024, "The Pages API response exceeded its size budget.")
                value = verifier.json_object(raw)
        except urllib.error.HTTPError as error:
            status = error.code
            error.close()
            if missing_ok and status == 404:
                return None
            raise InvalidSite("The scoped Pages API request failed.") from None
        except urllib.error.URLError:
            raise InvalidSite("The scoped Pages API could not be reached.") from None
        require(isinstance(value, dict) and value.get("success") is True and "result" in value,
                "The scoped Pages API request was unsuccessful.")
        return value["result"]

    def project(self, name):
        return self.request(name)

    def deployment(self, name, deployment_id):
        require(isinstance(deployment_id, str) and DEPLOYMENT_ID.fullmatch(deployment_id), "A Pages deployment ID is invalid.")
        return self.request(name, "/deployments/" + deployment_id)

    def deployments(self, name):
        return self.request(name, "/deployments?per_page=100")

    def rollback(self, name, deployment_id):
        require(isinstance(deployment_id, str) and DEPLOYMENT_ID.fullmatch(deployment_id), "A Pages rollback ID is invalid.")
        return self.request(name, "/deployments/" + deployment_id + "/rollback", method="POST")

    def domain(self, name, domain):
        return self.request(name, "/domains/" + domain, missing_ok=True)


def validate_project(value, name):
    require(isinstance(value, dict) and value.get("name") == name and value.get("id") and
            value.get("production_branch") == "main" and not value.get("source") and
            value.get("uses_functions") in (False, None) and isinstance(value.get("subdomain"), str) and
            re.fullmatch(r"[a-z0-9][a-z0-9-]*\.pages\.dev", value["subdomain"]),
            "Both existing targets must be static Direct Upload Pages projects on main.")
    return value


def validate_deployment(value, project, source_sha=None, marker=None):
    require(isinstance(value, dict) and isinstance(value.get("id"), str) and DEPLOYMENT_ID.fullmatch(value["id"]) and
            value.get("project_name") == project["name"] and value.get("project_id") == project["id"] and
            value.get("environment") == "production" and value.get("uses_functions") in (False, None),
            "The Pages deployment identity does not match its project.")
    trigger = value.get("deployment_trigger", {})
    require(isinstance(trigger, dict), "The Pages deployment trigger is invalid.")
    metadata = trigger.get("metadata", {})
    require(isinstance(metadata, dict) and trigger.get("type") == "ad_hoc" and metadata.get("branch") == "main",
            "The Pages deployment is not a Direct Upload production deployment.")
    verifier.commit(metadata.get("commit_hash"))
    require(source_sha is None or metadata.get("commit_hash") == source_sha, "The Pages source commit does not match.")
    require(marker is None or metadata.get("commit_message") == marker, "The Pages deployment belongs to another run.")
    return value


def canonical(client, project):
    current = validate_project(client.project(project["name"]), project["name"])
    require(current["id"] == project["id"] and current["subdomain"] == project["subdomain"],
            "The Pages project identity changed.")
    value = current.get("canonical_deployment")
    if value is None:
        return None
    require(isinstance(value, dict), "The Pages canonical deployment identity is invalid.")
    return validate_deployment(client.deployment(project["name"], value.get("id")), project)


def upload(wrangler, directory, name, source_sha, marker):
    with tempfile.TemporaryDirectory(prefix="cqlmatic-pages-") as work:
        work = Path(work)
        # An isolated working directory prevents repository config or Functions discovery.
        shutil.copytree(directory, work / "assets")
        env = {key: os.environ[key] for key in ("PATH", "SYSTEMROOT", "SSL_CERT_FILE", "SSL_CERT_DIR") if key in os.environ}
        env.update({"CI": "true", "XDG_CONFIG_HOME": str(work / "config"),
                    "CLOUDFLARE_API_TOKEN": os.environ["CLOUDFLARE_API_TOKEN"],
                    "CLOUDFLARE_ACCOUNT_ID": os.environ["CLOUDFLARE_ACCOUNT_ID"],
                    "WRANGLER_SEND_METRICS": "false", "WRANGLER_LOG_SANITIZE": "true",
                    "WRANGLER_LOG_PATH": str(work / "wrangler.log"),
                    "WRANGLER_OUTPUT_FILE_PATH": str(work / "output.jsonl")})
        command = [str(wrangler), "pages", "deploy", "assets", "--project-name", name,
                   "--branch", "main", "--commit-hash", source_sha, "--commit-message", marker,
                   "--commit-dirty=false"]
        try:
            result = subprocess.run(command, cwd=work, env=env, stdin=subprocess.DEVNULL,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=600)
            successful = result.returncode == 0
        except (OSError, subprocess.TimeoutExpired):
            successful = False
        output = work / "output.jsonl"
        records = [verifier.json_object(line) for line in output.read_text(encoding="utf-8").splitlines()] if output.is_file() else []
        matches = [item for item in records if item.get("type") == "pages-deploy" and item.get("pages_project") == name]
        require(len(matches) <= 1, "The Pages uploader returned ambiguous deployment identities.")
        return (matches[0].get("deployment_id") if matches else None), successful


def wait_deployment(client, project, deployment_id, source_sha, marker, timeout=180):
    deadline = time.monotonic() + timeout
    while True:
        value = validate_deployment(client.deployment(project["name"], deployment_id), project, source_sha, marker)
        stage = value.get("latest_stage", {})
        if stage.get("status") == "failure":
            raise InvalidSite("The Pages deployment did not reach a successful state.")
        if stage.get("name") == "deploy" and stage.get("status") == "success":
            return value
        require(time.monotonic() < deadline, "The Pages deployment exceeded its readiness budget.")
        time.sleep(5)


def served_path(name):
    if name == "404.html":
        return "/__cqlmatic_verify_missing_page__"
    if name == "index.html":
        return "/"
    if name.endswith("/index.html"):
        return "/" + name.removesuffix("index.html")
    return "/" + name.removesuffix(".html") if name.endswith(".html") else "/" + name


def verify_content(host, files, prefix, source_sha, deadline=None):
    parsed = urllib.parse.urlsplit(host)
    require(parsed.scheme == "https" and parsed.port in (None, 443) and parsed.username is None and
            parsed.password is None and parsed.path in ("", "/") and not parsed.query and not parsed.fragment,
            "The Pages verification host is invalid.")
    for path, content in sorted(files.items()):
        if not path.startswith(prefix + "/"):
            continue
        name = path[len(prefix) + 1:]
        if name in ("_headers", "_redirects"):
            continue
        if deadline is not None:
            require(time.monotonic() < deadline, "The custom-domain verification budget expired.")
        route = urllib.parse.quote(served_path(name), safe="/")
        url = host.rstrip("/") + route + "?cqlmatic_verify=" + source_sha
        request = urllib.request.Request(url, headers={"Accept-Encoding": "identity", "Cache-Control": "no-cache",
                                                       "User-Agent": "cqlmatic-pages-verifier"})
        try:
            response = urllib.request.build_opener(verifier.NoRedirect()).open(request, timeout=10)
        except urllib.error.HTTPError as error:
            if name != "404.html" or error.code != 404:
                error.close()
                raise InvalidSite("A Pages content check returned an unexpected status.") from None
            response = error
        except urllib.error.URLError:
            raise InvalidSite("A Pages HTTPS content check could not connect.") from None
        with response:
            require(response.status == (404 if name == "404.html" else 200), "A Pages content check returned an unexpected status.")
            actual = response.read(len(content) + 1)
        require(hashlib.sha256(actual).digest() == hashlib.sha256(content).digest(),
                "A deployed Pages file does not match the approved artifact.")


def deployment_host(value, project):
    url = value.get("url", "")
    host = urllib.parse.urlsplit(url).hostname or ""
    require(re.fullmatch(r"[a-z0-9]+\." + re.escape(project["subdomain"]), host),
            "The uploaded Pages URL does not match the project.")
    return url


def restore(client, projects, previous, attempted, changed, source_sha, marker):
    statuses = {}
    for name in reversed(attempted):
        try:
            project = projects[name]
            deployment_id = changed.get(name)
            if deployment_id is None:
                matches = [value for value in client.deployments(name)
                           if value.get("deployment_trigger", {}).get("metadata", {}).get("commit_message") == marker and
                           value.get("deployment_trigger", {}).get("metadata", {}).get("commit_hash") == source_sha]
                require(len(matches) <= 1, "The failed upload has ambiguous deployment identities.")
                if not matches:
                    statuses[name] = "no-upload-found"
                    continue
                deployment_id = validate_deployment(matches[0], project, source_sha, marker)["id"]
                changed[name] = deployment_id
            owned = validate_deployment(client.deployment(name, deployment_id), project, source_sha, marker)
            stage = owned.get("latest_stage", {})
            if stage.get("status") != "failure" and not (stage.get("name") == "deploy" and stage.get("status") == "success"):
                owned = wait_deployment(client, project, deployment_id, source_sha, marker)
            current = canonical(client, project)
            current_id = current.get("id") if current else None
            old = previous[name]
            if current_id == (old.get("id") if old else None):
                statuses[name] = "prior-production-unchanged"
                continue
            if current_id != deployment_id:
                statuses[name] = "blocked-newer-production"
                continue
            if old is None:
                statuses[name] = "unavailable-first-deployment"
                continue
            validate_deployment(client.deployment(name, old["id"]), project, old["deployment_trigger"]["metadata"]["commit_hash"])
            require(canonical(client, project)["id"] == deployment_id,
                    "Pages production changed immediately before restoration.")
            restored = validate_deployment(client.rollback(name, old["id"]), project)
            require(restored["id"] == old["id"] and canonical(client, project)["id"] == old["id"],
                    "The previous Pages deployment was not restored.")
            statuses[name] = "restored-" + old["id"]
        except (InvalidSite, OSError, ValueError, KeyError, TypeError, AttributeError):
            statuses[name] = "restoration-incomplete"
    return statuses


def write_summary(proof):
    raw = json.dumps(proof, sort_keys=True, indent=2)
    print(raw, flush=True)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as output:
            output.write("Pages production result\n\n```json\n" + raw + "\n```\n")


def deploy_sites(client, dist, files, source_sha, manifest_sha256, wrangler, marker, domain_timeout,
                 upload_site=upload, check_content=verify_content):
    projects, previous, attempted, changed = {}, {}, [], {}
    proof = {"source_commit": source_sha, "site_manifest_sha256": manifest_sha256,
             "deployments": changed, "previous_deployments": {}, "status": "failed"}
    try:
        for name, _, _ in PROJECTS:
            project = validate_project(client.project(name), name)
            projects[name] = project
            old = canonical(client, project)
            if old:
                require(old.get("latest_stage", {}).get("name") == "deploy" and
                        old.get("latest_stage", {}).get("status") == "success", "Previous Pages production is not healthy.")
            previous[name] = old
            proof["previous_deployments"][name] = old["id"] if old else None
        for name, prefix, _ in PROJECTS:
            current = canonical(client, projects[name])
            require((current["id"] if current else None) == (previous[name]["id"] if previous[name] else None),
                    "Pages production changed before this upload.")
            attempted.append(name)
            deployment_id, successful = upload_site(wrangler, dist / prefix, name, source_sha, marker)
            if deployment_id is not None:
                require(isinstance(deployment_id, str) and DEPLOYMENT_ID.fullmatch(deployment_id), "The uploader returned an invalid Pages ID.")
                changed[name] = deployment_id
            require(successful and deployment_id is not None, "The Pages upload failed.")
            value = wait_deployment(client, projects[name], deployment_id, source_sha, marker)
            require(canonical(client, projects[name])["id"] == deployment_id, "Pages production changed during upload verification.")
            check_content(deployment_host(value, projects[name]), files, prefix, source_sha)
        print("CQLMATIC_PAGES_UPLOADS_VERIFIED", flush=True)
        print("Both approved uploads are verified; waiting for custom-domain association and HTTPS activation.", flush=True)
        deadline = time.monotonic() + domain_timeout
        while True:
            try:
                for name, prefix, domain in PROJECTS:
                    require(canonical(client, projects[name])["id"] == changed[name], "Pages production changed during domain verification.")
                    value = client.domain(name, domain)
                    require(value is not None and value.get("name") == domain and value.get("status") == "active" and
                            value.get("validation_data", {}).get("status") == "active" and
                            value.get("verification_data", {}).get("status") == "active", "The Pages custom domain is not active.")
                    check_content("https://" + domain, files, prefix, source_sha, deadline)
                break
            except (InvalidSite, OSError):
                require(time.monotonic() < deadline, "Custom-domain activation or HTTPS verification exceeded 30 minutes.")
                time.sleep(min(15, max(0, deadline - time.monotonic())))
        proof["status"] = "verified"
        return proof
    except (InvalidSite, OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        proof["failure"] = str(error) if isinstance(error, InvalidSite) else "A deployment operation failed."
        proof["restoration"] = restore(client, projects, previous, attempted, changed, source_sha, marker)
        return proof


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dist", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--wrangler", type=Path, required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-attempt", required=True)
    args = parser.parse_args()
    verifier.commit(args.source_sha)
    run_id, attempt = verifier.positive_id(args.run_id), verifier.positive_id(args.run_attempt)
    files = verifier.check_directory(args.dist, args.source_sha, args.manifest_sha256)
    wrangler = args.wrangler.resolve(strict=True)
    package = verifier.json_object((wrangler.parent.parent / "package.json").read_bytes())
    require(package.get("name") == "wrangler" and package.get("version") == "4.158.0",
            "The deployment requires the locked Wrangler version.")
    client = Cloudflare(os.environ.get("CLOUDFLARE_ACCOUNT_ID"), os.environ.get("CLOUDFLARE_API_TOKEN"))
    proof = deploy_sites(client, args.dist.resolve(), files, args.source_sha, args.manifest_sha256,
                         wrangler, f"CQLmatic Pages {run_id}/{attempt}", 1800)
    write_summary(proof)
    require(proof["status"] == "verified", "Pages production verification failed; inspect the restoration result.")


if __name__ == "__main__":
    try:
        main()
    except (InvalidSite, OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        print(str(error) if isinstance(error, InvalidSite) else "Pages production deployment failed.", file=sys.stderr)
        sys.exit(1)
