#!/usr/bin/env python3
"""Validate, install and verify Flowerly release status-check rulesets.

No network is used by 'validate'. Remote 'apply' needs an explicit opt-in
and a GitHub token with repository administration/rulesets permission.
"""
import argparse
import json
import os
from pathlib import Path
import re
import sys
from urllib import error, parse, request

ROOT = Path(__file__).resolve().parent.parent
POLICY_FILE = ".github/release-gates.json"
REPOSITORY = "Doldas/flowerly_os"
EXPECTED_CONTEXTS = (
    "rust",
    "docs",
    "Validate branch policy and issue link",
    "cargo-audit",
    "cargo-deny (advisories)",
    "cargo-deny (bans licenses sources)",
    "CodeQL (rust)",
)
ACTIONS_APP_ID = 15368


def required_rule(policy):
    matches = [rule for rule in policy["rules"]
               if rule["type"] == "required_status_checks"]
    if len(matches) != 1:
        raise ValueError("exactly one required_status_checks rule is required")
    return matches[0]["parameters"]


def validate(root=ROOT):
    """Fail closed on missing, skipped, renamed or bypassable checks."""
    policy = json.loads((root / POLICY_FILE).read_text(encoding="utf-8"))
    if policy.get("name") != "FLOWERLY_RELEASE_REQUIRED_CHECKS":
        raise ValueError("unexpected ruleset name")
    if policy.get("target") != "branch" or policy.get("enforcement") != "active":
        raise ValueError("ruleset must actively enforce branch rules")
    if policy.get("bypass_actors") != []:
        raise ValueError("release checks must not have bypass actors")
    includes = policy.get("conditions", {}).get("ref_name", {}).get("include")
    if includes != ["refs/heads/release/*"]:
        raise ValueError("ruleset must match release/* only")
    if policy.get("conditions", {}).get("ref_name", {}).get("exclude") != []:
        raise ValueError("release ruleset cannot exclude branches")
    if len(policy.get("rules", [])) != 2:
        raise ValueError("only PR and required-check rules are expected")
    pull_rules = [r for r in policy["rules"] if r.get("type") == "pull_request"]
    if len(pull_rules) != 1:
        raise ValueError("release branch must require pull requests")
    if pull_rules[0].get("parameters", {}).get("required_approving_review_count") != 0:
        raise ValueError("release approval count must be explicit")
    if not pull_rules[0].get("parameters", {}).get("required_review_thread_resolution"):
        raise ValueError("review threads must be resolved")
    status = required_rule(policy)
    if status.get("strict_required_status_checks_policy") is not True:
        raise ValueError("check latest merge base with strict status policy")
    if status.get("do_not_enforce_on_create") is not False:
        raise ValueError("checks must also apply when a branch is created")
    checks = status.get("required_status_checks", [])
    contexts = [item.get("context") for item in checks]
    if tuple(contexts) != EXPECTED_CONTEXTS or len(set(contexts)) != len(contexts):
        raise ValueError(f"required check contexts drifted: {contexts}")
    if any(item.get("integration_id") != ACTIONS_APP_ID for item in checks):
        raise ValueError("pin required checks to the observed GitHub Actions app")

    def workflow(name):
        return (root / ".github/workflows" / name).read_text(encoding="utf-8")

    ci = workflow("ci.yml")
    security = workflow("security.yml")
    codeql = workflow("codeql.yml")
    pr = workflow("pr-validation.yml")
    promotion = workflow("release-promotion-validation.yml")
    for name, content in (("ci", ci), ("security", security),
                          ("codeql", codeql)):
        if not re.search(r"(?m)^  pull_request:\s*$", content):
            raise ValueError(f"{name}: PR trigger required on release branches")
        if "branches: [master, 'release/**']" not in content:
            raise ValueError(f"{name}: master and release trigger required")
    for name, content in (("security", security), ("codeql", codeql)):
        if "paths-ignore:" in content or re.search(r"(?m)^\s+paths:\s*$", content):
            raise ValueError(f"{name}: mandatory job cannot be skipped by file path")
    if not re.search(r"(?m)^  pull_request:\s*$", pr) or "branches: ['release/**']" not in pr:
        raise ValueError("pr-validation: release-only PR trigger required")
    if "branches: [master]" in pr:
        raise ValueError("pr-validation must not run for master promotions")
    if not all(f"startsWith(github.head_ref, '{prefix}/')" in pr for prefix in ("feature", "bugfix", "docs")):
        raise ValueError("pr-validation must select development branches")
    if "branches: [master]" not in promotion or "verify-master" not in promotion:
        raise ValueError("exact-SHA master evidence gate missing")
    if "name: rust" not in ci or "name: docs" not in ci:
        raise ValueError("CI must report rust and docs check names")
    if "name: Validate branch policy and issue link" not in pr:
        raise ValueError("PR validation check was renamed")
    forward = workflow("branch-forward.yml")
    if "FLOWERLY_FORWARD_BOT_TOKEN" not in forward or "branch-forward.py reconcile" not in forward:
        raise ValueError("bot branch propagation workflow missing or unprotected")
    if "name: cargo-audit" not in security or "name: cargo-deny" not in security:
        raise ValueError("security job names changed")
    for matrix in ("- advisories", "- bans licenses sources"):
        if matrix not in security:
            raise ValueError(f"security matrix missing {matrix}")
    if "continue-on-error:" in security:
        raise ValueError("cargo-deny advisories must fail CI, never soft-fail")
    if "run: cargo audit" not in security or "cargo deny --all-features check" not in security:
        raise ValueError("cargo audit and deny must actually execute")
    if "name: CodeQL" not in codeql or "language: [rust]" not in codeql:
        raise ValueError("CodeQL (rust) check must be generated")
    if "python3 scripts/release-gates.py validate" not in ci:
        raise ValueError("CI must continuously validate the release gate policy")
    return policy


def api(method, path, payload=None):
    token = os.getenv("GH_TOKEN") or os.getenv("GITHUB_TOKEN")
    if not token:
        raise ValueError("set GH_TOKEN or GITHUB_TOKEN with GitHub ruleset permissions")
    url = "https://api.github.com/repos/" + REPOSITORY + path
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    req = request.Request(
        url, data=data, method=method,
        headers={
            "Authorization": "Bearer " + token,
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "flowerly-release-gates",
            "Content-Type": "application/json",
        },
    )
    try:
        with request.urlopen(req, timeout=20) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    except error.HTTPError as exc:
        detail = exc.read().decode("utf-8", errors="replace")[:500]
        raise RuntimeError(f"GitHub API HTTP {exc.code}: {detail}") from exc


def find_policy_ruleset():
    policies = api("GET", "/rulesets")
    items = [entry for entry in policies
             if entry.get("name") == "FLOWERLY_RELEASE_REQUIRED_CHECKS"]
    if len(items) > 1:
        raise ValueError("multiple rulesets share the managed name")
    return items[0]["id"] if items else None


def assert_rule_enforcement(policy, actual):
    if actual.get("enforcement") != "active":
        raise ValueError("remote ruleset is not active")
    if actual.get("bypass_actors") != []:
        raise ValueError("remote ruleset permits bypass")
    if actual.get("conditions", {}).get("ref_name") != policy["conditions"]["ref_name"]:
        raise ValueError("remote ruleset has wrong branch targets")
    required = required_rule(actual)
    desired = required_rule(policy)
    if required.get("strict_required_status_checks_policy") is not True:
        raise ValueError("remote status checks are not strict")
    if required.get("do_not_enforce_on_create") is not False:
        raise ValueError("remote create bypass is enabled")
    observed = {(item.get("context"), item.get("integration_id"))
                for item in required.get("required_status_checks", [])}
    expected = {(item["context"], item["integration_id"])
                for item in desired["required_status_checks"]}
    if observed != expected:
        raise ValueError(f"remote required check list mismatch: {observed}")
    if not any(item.get("type") == "pull_request" for item in actual.get("rules", [])):
        raise ValueError("remote ruleset does not require pull requests")


def verify(policy):
    ruleset_id = find_policy_ruleset()
    if ruleset_id is None:
        raise ValueError("release ruleset not installed; cannot claim protection")
    actual = api("GET", f"/rulesets/{ruleset_id}")
    assert_rule_enforcement(policy, actual)
    # GitHub computes effective branch rules from ALL applicable rulesets.
    for branch in ("release/2",):
        encoded = parse.quote(branch, safe="")
        effective = api("GET", f"/rules/branches/{encoded}")
        if isinstance(effective, dict):
            effective = effective.get("rules", [])
        if not isinstance(effective, list):
            raise ValueError(f"bad effective rules payload for {branch}")
        candidates = [r for r in effective if r.get("type") == "required_status_checks"]
        expected = {c["context"] for c in
                    required_rule(policy)["required_status_checks"]}
        if not any(expected <= {c.get("context") for c in
                    r.get("parameters", {}).get("required_status_checks", [])}
                   for r in candidates):
            raise ValueError(f"{branch}: GitHub effective rules omit required checks")
        if not any(r.get("type") == "pull_request" for r in effective):
            raise ValueError(f"{branch}: effective rules do not require a PR")
        print(f"{branch}: required checks and pull request rules active")
    print(f"verified ruleset {ruleset_id} (no bypass actors)")



def verify_probe(policy, pr_number):
    """Verify a real failing required check blocks a non-draft PR, without merging."""
    pr = api("GET", f"/pulls/{pr_number}")
    if pr.get("state") != "open" or pr.get("draft"):
        raise ValueError("negative probe must be an open non-draft PR")
    if pr.get("base", {}).get("ref") != "release/2":
        raise ValueError("probe must target protected release/2")
    if pr.get("mergeable_state") != "blocked":
        raise ValueError(f"probe is not blocked: {pr.get('mergeable_state')}")
    head = pr["head"]["sha"]
    check_runs = api("GET", f"/commits/{head}/check-runs?per_page=100")
    required = {item["context"] for item in
                required_rule(policy)["required_status_checks"]}
    failures = [
        item for item in check_runs.get("check_runs", [])
        if item.get("name") in required
        and item.get("app", {}).get("id") == ACTIONS_APP_ID
        and item.get("conclusion") in ("failure", "timed_out", "cancelled")
    ]
    if not failures:
        raise ValueError("probe has no failing required check on its current head")
    print(f"negative blocking proof: PR {pr_number}, head {head}, "
          f"failed {[item['name'] for item in failures]}, mergeable_state=blocked")

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("validate", "verify", "apply"))
    parser.add_argument("--probe-pr", type=int,
                        help="After verify, prove a failing required check blocks an open PR")
    parser.add_argument("--confirm-apply", action="store_true",
                        help="Explicitly authorize creating/updating GitHub ruleset")
    args = parser.parse_args()
    try:
        policy = validate()
        if args.command == "validate":
            print("release ruleset config and mandatory workflows validated")
        elif args.command == "verify":
            verify(policy)
            if args.probe_pr is not None:
                verify_probe(policy, args.probe_pr)
        else:
            if not args.confirm_apply:
                raise ValueError("apply requires --confirm-apply and an admin-scoped token")
            ruleset_id = find_policy_ruleset()
            if ruleset_id is None:
                installed = api("POST", "/rulesets", policy)
                print(f"created required-check ruleset {installed['id']}")
            else:
                api("PUT", f"/rulesets/{ruleset_id}", policy)
                print(f"updated required-check ruleset {ruleset_id}")
            verify(policy)
    except (ValueError, RuntimeError, OSError, json.JSONDecodeError) as exc:
        print(f"release gate error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
