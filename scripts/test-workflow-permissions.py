#!/usr/bin/env python3
"""Least-privilege GITHUB_TOKEN policy for repository workflows (#187).

Every workflow declares a top-level `permissions:` block that grants no
write scope, so a new job never inherits the repository default token.
Write scopes are granted per job and only where this table records why.

Offline structural check of the block-style YAML used in this repository;
any permissions syntax it does not understand fails closed.
"""
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"

# (workflow, job, scope): why the job needs write access.
ALLOWED_WRITES = {
    ("codeql.yml", "analyze", "security-events"): "upload CodeQL SARIF",
    ("docs-pages.yml", "build", "pages"): "configure-pages on master",
    ("docs-pages.yml", "deploy", "pages"): "deploy-pages on master",
    ("docs-pages.yml", "deploy", "id-token"): "deploy-pages OIDC on master",
    ("release.yml", "release", "contents"): "publish the GitHub release for a v* tag",
    ("scorecard.yml", "analysis", "security-events"): "upload Scorecard SARIF",
    ("scorecard.yml", "analysis", "id-token"): "publish Scorecard results",
}
LEVELS = {"read", "write", "none"}
KEY = re.compile(r"^( *)([A-Za-z0-9_-]+):(.*)$")


def strip_comment(text):
    return re.sub(r"\s+#.*$", "", text).strip()


def permission_blocks(text):
    """Yield (job or None, {scope: level} or error string) for each block."""
    lines = [line for line in text.splitlines()
             if line.strip() and not line.lstrip().startswith("#")]
    job, in_jobs = None, False
    for index, line in enumerate(lines):
        found = KEY.match(line)
        if not found:
            continue
        indent, name, rest = len(found.group(1)), found.group(2), strip_comment(found.group(3))
        if indent == 0:
            in_jobs, job = name == "jobs", None
        elif in_jobs and indent == 2:
            job = name
        if name != "permissions" or not (indent == 0 or (job and indent == 4)):
            continue
        owner = None if indent == 0 else job
        if rest in ("{}", "read-all"):
            yield owner, {}
            continue
        if rest:
            yield owner, f"unsupported permissions value {rest!r}"
            continue
        scopes = {}
        for child in lines[index + 1:]:
            nested = KEY.match(child)
            if not nested or len(nested.group(1)) <= indent:
                break
            scopes[nested.group(2)] = strip_comment(nested.group(3))
        yield owner, scopes


def problems_of(name, text):
    problems = []
    blocks = list(permission_blocks(text))
    if not any(owner is None for owner, _ in blocks):
        problems.append(f"{name}: no top-level permissions block")
    for owner, scopes in blocks:
        where = f"{name}:{owner or 'top-level'}"
        if isinstance(scopes, str):
            problems.append(f"{where}: {scopes}")
            continue
        for scope, level in scopes.items():
            if level not in LEVELS:
                problems.append(f"{where}: {scope} has unsupported level {level!r}")
            elif level == "write" and owner is None:
                problems.append(f"{where}: grants {scope}: write to every job")
            elif level == "write" and (name, owner, scope) not in ALLOWED_WRITES:
                problems.append(f"{where}: {scope}: write is not in ALLOWED_WRITES")
    return problems


def main() -> int:
    files = sorted(WORKFLOWS.glob("*.yml")) + sorted(WORKFLOWS.glob("*.yaml"))
    errors = []
    for path in files:
        errors += problems_of(path.name, path.read_text(encoding="utf-8"))
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"workflow permissions least-privilege: {len(files)} files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
