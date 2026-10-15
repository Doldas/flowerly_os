#!/usr/bin/env python3
"""Protected release forwarding; master is only promoted from verified releases."""
import argparse
import json
import os
import re
import sys
from urllib import error, parse, request

RELEASE = re.compile(r"^release/([1-9][0-9]*)$")
DEV = re.compile(r"^(feature|bugfix|docs)/.+$")
ARCH = "Release PTY kernel cgroup, escaped descendant and Arch guest acceptance"
ISSUE = 112


def version(branch):
    match = RELEASE.fullmatch(branch)
    return int(match.group(1)) if match else None


def releases(names):
    pairs = sorted((version(x), x) for x in names if version(x) is not None)
    numbers = [number for number, _ in pairs]
    if len(set(numbers)) != len(numbers):
        raise ValueError("duplicate numeric release branches")
    for a, b in zip(numbers, numbers[1:]):
        if b != a + 1:
            raise ValueError(f"missing release/{a + 1} between release/{a} and release/{b}")
    return [name for _, name in pairs]


def permitted(head, base):
    if base == "master":
        return version(head) is not None
    destination = version(base)
    if destination is None:
        return False
    origin = version(head)
    return bool(DEV.fullmatch(head)) or (origin is not None and origin + 1 == destination)


def api(method, path, data=None):
    token = os.getenv("GITHUB_TOKEN")
    repo = os.getenv("GITHUB_REPOSITORY", "")
    if not token or not re.fullmatch(r"[\w.-]+/[\w.-]+", repo):
        raise ValueError("GITHUB_TOKEN and GITHUB_REPOSITORY required")
    body = json.dumps(data).encode() if data is not None else None
    req = request.Request(
        "https://api.github.com/repos/" + repo + path,
        data=body, method=method,
        headers={"Authorization": "Bearer " + token,
                 "Accept": "application/vnd.github+json",
                 "Content-Type": "application/json",
                 "X-GitHub-Api-Version": "2022-11-28",
                 "User-Agent": "flowerly-forward-branches"},
    )
    try:
        with request.urlopen(req, timeout=30) as response:
            raw = response.read()
            return json.loads(raw) if raw else {}
    except error.HTTPError as exc:
        detail = exc.read().decode(errors="replace")[:500]
        raise ValueError(f"GitHub API HTTP {exc.code}: {detail}") from exc


def graphql(query, variables):
    req = request.Request(
        "https://api.github.com/graphql", method="POST",
        data=json.dumps({"query": query, "variables": variables}).encode(),
        headers={"Authorization": "Bearer " + os.environ["GITHUB_TOKEN"],
                 "Accept": "application/vnd.github+json",
                 "Content-Type": "application/json",
                 "User-Agent": "flowerly-forward-branches"},
    )
    with request.urlopen(req, timeout=30) as response:
        payload = json.load(response)
    if payload.get("errors"):
        raise ValueError(f"GitHub auto-merge rejected: {payload['errors']}")
    return payload["data"]


def pages(path, client=api):
    items = []
    for index in range(1, 11):
        separator = "&" if "?" in path else "?"
        part = client("GET", f"{path}{separator}per_page=100&page={index}")
        if not isinstance(part, list):
            raise ValueError(f"unexpected paginated response: {path}")
        items.extend(part)
        if len(part) < 100:
            return items
    raise ValueError("over 1000 API items; refusing incomplete branch list")


def branch_names(client=api):
    return releases([b["name"] for b in pages("/branches", client)])


def successful_run(workflow, branch, sha, client=api):
    base = (f"/actions/workflows/{workflow}/runs?branch="
            + parse.quote(branch, safe="") + "&event=push")
    runs = []
    for index in range(1, 4):
        chunk = client("GET", f"{base}&per_page=100&page={index}").get("workflow_runs", [])
        runs.extend(chunk)
        if len(chunk) < 100:
            break
    for run in runs:
        if (run.get("head_sha") == sha
                and run.get("head_branch") == branch
                and run.get("event") == "push"
                and run.get("status") == "completed"
                and run.get("conclusion") == "success"):
            return run
    raise ValueError(f"{workflow}: no successful completed PUSH run on {branch}@{sha}")


def verify_master(head, sha, client=api):
    names = branch_names(client)
    if not names or head != names[-1]:
        raise ValueError(f"only latest release {names[-1] if names else '(none)'} can enter master")
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("exact lowercase 40-digit SHA required")
    current = client("GET", "/branches/" + parse.quote(head, safe=""))
    if current["commit"]["sha"] != sha:
        raise ValueError("source branch moved: stale PR head")
    ci = successful_run("ci.yml", head, sha, client)
    successful_run("security.yml", head, sha, client)
    successful_run("codeql.yml", head, sha, client)
    jobs = []
    for page in range(1, 11):
        payload = client("GET", f"/actions/runs/{ci['id']}/jobs?per_page=100&page={page}")
        part = payload.get("jobs", [])
        jobs.extend(part)
        if len(part) < 100:
            break
    else:
        raise ValueError("too many CI jobs for exhaustive Arch verification")
    rust = [job for job in jobs if job.get("name") == "rust" and job.get("conclusion") == "success"]
    docs = [job for job in jobs if job.get("name") == "docs" and job.get("conclusion") == "success"]
    if not rust or not docs:
        raise ValueError("release push CI rust and docs jobs must both pass")
    if not any(step.get("name") == ARCH and step.get("conclusion") == "success"
               and step.get("status") == "completed"
               for job in rust for step in job.get("steps", [])):
        raise ValueError("exact-head Arch VM acceptance was not executed successfully")
    return True


def ensure_pr(head, base, draft, auto_merge, client=api, gql=graphql):
    if not permitted(head, base):
        raise ValueError(f"forbidden direction: {head} -> {base}")
    comparison = client("GET", "/compare/" + parse.quote(base, safe="")
                        + "..." + parse.quote(head, safe=""))
    if comparison.get("ahead_by", 0) == 0 or not comparison.get("files"):
        print(f"{head} -> {base}: no unapplied file changes")
        return
    owner = os.environ["GITHUB_REPOSITORY"].split("/")[0]
    find = ("/pulls?state=open&base=" + parse.quote(base, safe="")
            + "&head=" + parse.quote(owner + ":" + head, safe=""))
    existing = client("GET", find)
    if existing:
        pr = existing[0]
        print(f"existing PR #{pr['number']}: {head} -> {base}")
    else:
        promotion = base == "master"
        body = (f"Relates to #{ISSUE}.\n\n"
                f"Automated {'stable promotion candidate' if promotion else 'forward-merge'} "
                f"from {head} to {base}.\n\n"
                + ("**Draft only.** Require exact-head release push CI with real Arch VM, "
                   "security and CodeQL, required PR checks, and explicit maintainer "
                   "approval. NEVER auto-merge to master."
                   if promotion else
                   "Protected forward merge: required PR checks and conflict resolution "
                   "apply. Use MERGE commits to preserve release ancestry."))
        pr = client("POST", "/pulls", {
            "head": head, "base": base,
            "title": f"{'Promote' if promotion else 'Forward-merge'} {head} into {base}",
            "body": body, "draft": draft,
        })
        print(f"opened PR #{pr['number']}: {head} -> {base}")
    if base == "master":
        return  # Absolutely never enable automatic master promotion.
    if auto_merge and not pr.get("draft") and not pr.get("auto_merge"):
        gql(
            "mutation($id:ID!){enablePullRequestAutoMerge(input:"
            "{pullRequestId:$id,mergeMethod:MERGE}){pullRequest{number}}}",
            {"id": pr["node_id"]},
        )
        print(f"auto-merge enabled on PR #{pr['number']} (protected checks still required)")


def reconcile(client=api, gql=graphql):
    configuration = client("GET", "")
    names = branch_names(client)
    if not names:
        raise ValueError("no release branches")
    auto = configuration.get("allow_auto_merge") is True
    if not auto and len(names) > 1:
        print("NOTICE: repository Allow auto-merge is disabled; forward PRs "
              "will require a manual merge until enabled")
    for older, newer in zip(names, names[1:]):
        ensure_pr(older, newer, False, auto, client, gql)
    ensure_pr(names[-1], "master", True, False, client, gql)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("reconcile")
    check = sub.add_parser("verify-master")
    check.add_argument("--head", required=True)
    check.add_argument("--sha", required=True)
    args = parser.parse_args()
    try:
        if args.command == "reconcile":
            reconcile()
        else:
            verify_master(args.head, args.sha)
            print(f"release promotion verified: {args.head}@{args.sha}")
    except (ValueError, RuntimeError, KeyError, error.URLError, OSError) as exc:
        print(f"branch-forward error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
