#!/usr/bin/env python3
"""Offline regression checks for project sync trigger and token boundaries (#249)."""
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
AUTO = (ROOT / ".github/workflows/project-sync.yml").read_text(encoding="utf-8")
MANUAL = (ROOT / ".github/workflows/project-sync-manual.yml").read_text(encoding="utf-8")


def main():
    assert "github.event_name != 'schedule' || github.ref == format('refs/heads/{0}', github.event.repository.default_branch)" in AUTO
    assert "github.event.repository.default_branch == github.ref" not in AUTO
    for name, content in (("automatic", AUTO), ("manual", MANUAL)):
        assert "secrets.FLOWERLY_PROJECT_TOKEN" in content, name
        assert "secrets.FLOWERLY_PROJECT_TOKEN || secrets.GITHUB_TOKEN" not in content, name
        assert "Require project-write credential" in content, name
        assert 'if [[ -z "$PROJECT_TOKEN" ]]' in content, name
        assert "exit 1" in content, name
        assert "contents: read" in content and "issues: read" in content, name
        assert "permissions: write-all" not in content, name
        assert "FLOWERLY_PROJECT_TOKEN ||" not in content, name
    print("project sync trigger and token boundary checks passed")


if __name__ == "__main__":
    main()
