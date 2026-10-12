#!/usr/bin/env python3
"""Bootstrap the native Flowerly OS GitHub Project v2 board.

Requires GitHub CLI authenticated with 'project' scope:
  gh auth refresh -s project
  python3 scripts/bootstrap-github-project.py

Safe to rerun: reuses the project, skips existing items, and never edits issues/PRs.
"""
import json
import re
import subprocess
import sys

OWNER = "Doldas"
REPO = "Doldas/flowerly_os"
TITLE = "Flowerly OS — Engineering Roadmap"
CURRENT = "0.0.4"
RELEASES = ["0.0.2", "0.0.3", "0.0.4", "0.0.5", "0.0.6",
            "0.0.7", "0.0.8", "0.0.9", "0.1.0", "Unscheduled"]
SPRINT_RELEASE = {1: "0.0.2", 2: "0.0.4", 3: "0.0.6", 4: "0.0.8", 31: "0.0.4"}
VIEWS = [("Roadmap", "TABLE_LAYOUT"),
         ("Current Sprint", "BOARD_LAYOUT"),
         ("Release Board", "BOARD_LAYOUT"),
         ("Architecture / Research", "TABLE_LAYOUT"),
         ("Bugs & Security", "TABLE_LAYOUT")]


def gh(*args):
    p = subprocess.run(["gh", *map(str, args)], text=True,
                       capture_output=True, check=False)
    if p.returncode:
        raise RuntimeError("gh " + " ".join(map(str, args[:4])) +
                           " failed: " + p.stderr.strip())
    return p.stdout.strip()


def data(*args):
    return json.loads(gh(*args))


def ensure_project():
    projects = data("project", "list", "--owner", OWNER, "--limit", "100",
                    "--format", "json").get("projects", [])
    match = next((p for p in projects if p["title"] == TITLE), None)
    if match:
        number = match["number"]
        print("Reusing project", number)
    else:
        number = data("project", "create", "--owner", OWNER, "--title", TITLE,
                      "--format", "json")["number"]
        print("Created project", number)
    # Linking an already-linked project is harmless, but some CLI versions error.
    try:
        gh("project", "link", number, "--owner", OWNER, "--repo", "flowerly_os")
    except RuntimeError as e:
        if "already" not in str(e).lower():
            raise
    gh("project", "edit", number, "--owner", OWNER, "--description",
       "Flowerly OS release-gated roadmap; see docs/roadmap.md and "
       "docs/review-implementation-plan-2026-10-12.md on release/2.")
    project_id = data("project", "view", number, "--owner", OWNER,
                      "--format", "json")["id"]
    return number, project_id


def ensure_field(number, name, field_type, choices=None):
    fields = data("project", "field-list", number, "--owner", OWNER,
                  "--limit", "100", "--format", "json")["fields"]
    if any(f["name"] == name for f in fields):
        return
    args = ["project", "field-create", number, "--owner", OWNER,
            "--name", name, "--data-type", field_type]
    if choices:
        args += ["--single-select-options", ",".join(choices)]
    gh(*args)
    print("Added field:", name)


def ensure_views(number, project_id):
    query = """query($login:String!,$num:Int!){
      user(login:$login){projectV2(number:$num){views(first:100){nodes{name}}}}
    }"""
    existing = data("api", "graphql", "-f", "query=" + query,
                    "-f", "login=" + OWNER, "-F", "num=" + str(number))
    names = {v["name"] for v in
             existing["data"]["user"]["projectV2"]["views"]["nodes"]}
    mutation = """mutation($id:ID!,$name:String!,$layout:ProjectV2ViewLayout!){
      createProjectV2View(input:{projectId:$id,name:$name,layout:$layout}){
        projectV2View{id}
      }
    }"""
    for name, layout in VIEWS:
        if name not in names:
            data("api", "graphql", "-f", "query=" + mutation,
                 "-f", "id=" + project_id, "-f", "name=" + name,
                 "-f", "layout=" + layout)
            print("Added view:", name)


def release(item):
    if item["number"] in SPRINT_RELEASE:
        return SPRINT_RELEASE[item["number"]]
    title = item.get("title", "")
    milestone = (item.get("milestone") or {}).get("title") or ""
    m = re.search(r"0[.]0[.][2-9]|0[.]1[.]0", title + " " + milestone)
    return m.group(0) if m else "Unscheduled"


def fields(item, is_pr):
    name = item["title"].lower()
    labels = [v["name"].lower() for v in item.get("labels", [])]
    rel = release(item)
    closed = item["state"].upper() != "OPEN"
    if closed:
        stage = "Done"
    elif is_pr:
        stage = "Review"
    elif item["number"] == 15 or rel == "0.0.2":
        stage = "Ready"
    elif rel == CURRENT:
        stage = "In progress"
    else:
        stage = "Backlog"

    if is_pr:
        typ = "Pull request"
    elif name.startswith("[roadmap]"):
        typ = "Roadmap"
    elif item["number"] == 15:
        typ = "Planning"
    elif "security" in name:
        typ = "Security"
    elif "type:test" in labels:
        typ = "Test"
    elif "bug" in name or "type:bug" in labels:
        typ = "Bug"
    elif "docs" in name or "documentation" in name:
        typ = "Docs"
    else:
        typ = "Feature"

    areas = [v.split(":", 1)[1].capitalize() for v in labels
             if v.startswith("area:")]
    area = areas[0] if areas else (
        "Security" if "security" in name or "hardening" in name else
        "CI" if "ci " in name or "github" in name or "test" in name else
        "Image" if "arch" in name or "image" in name else
        "Docs" if "docs" in name else
        "Planning" if item["number"] == 15 else "Other")
    allowed = {"Core", "Flow", "Session", "Storage", "Evolution", "Goal",
               "Agent", "Image", "Docs", "CI", "Security", "Planning", "Other"}
    if area not in allowed:
        area = "Other"
    priorities = [v.split(":", 1)[1].upper() for v in labels
                  if v.startswith("priority:")]
    priority = priorities[0] if priorities and priorities[0] in {
        "P0", "P1", "P2", "P3"} else "Unassigned"
    sprint = re.search(r"Sprint[ ]+([0-9]+)", item["title"], flags=re.I)
    owners = ", ".join(a["login"] for a in item.get("assignees", []))
    return {"Target release": rel, "Delivery stage": stage,
            "Type": typ, "Area": area, "Priority": priority,
            "Risk": "Unassessed",
            "Sprint": "Sprint " + sprint.group(1) if sprint else "",
            "Owner": owners}


def sync(number):
    issues = data("issue", "list", "--repo", REPO, "--state", "all",
                  "--limit", "1000", "--json",
                  "number,title,state,labels,milestone,assignees")
    prs = data("pr", "list", "--repo", REPO, "--state", "all",
               "--limit", "1000", "--json", "number,title,state,mergedAt")
    existing = data("project", "item-list", number, "--owner", OWNER,
                    "--limit", "1000", "--format", "json").get("items", [])
    urls = {p.get("content", {}).get("url") for p in existing
            if isinstance(p.get("content"), dict)}
    added = 0
    for is_pr, items, path in [(False, issues, "issues"), (True, prs, "pull")]:
        for item in items:
            url = "https://github.com/" + REPO + "/" + path + "/" + str(item["number"])
            if url in urls:
                continue  # Preserve human-edited Project field values.
            gh("project", "item-add", number, "--owner", OWNER, "--url", url)
            urls.add(url)
            added += 1
            for key, value in fields(item, is_pr).items():
                if not value:
                    continue
                args = ["project", "item-edit", number, "--owner", OWNER,
                        "--url", url, "--field", key]
                args += ["--text", value] if key in ("Sprint", "Owner") else [
                    "--value", value]
                gh(*args)
            print("Synced", path, item["number"])
    print("Found", len(issues), "issues and", len(prs),
          "PRs;", added, "new Project items. Existing items were left unchanged.")


def main():
    gh("auth", "status")
    number, project_id = ensure_project()
    ensure_field(number, "Target release", "SINGLE_SELECT", RELEASES)
    ensure_field(number, "Delivery stage", "SINGLE_SELECT",
                 ["Backlog", "Ready", "In progress", "Review", "Done"])
    ensure_field(number, "Sprint", "TEXT")
    ensure_field(number, "Area", "SINGLE_SELECT",
                 ["Core", "Flow", "Session", "Storage", "Evolution", "Goal",
                  "Agent", "Image", "Docs", "CI", "Security", "Planning", "Other"])
    ensure_field(number, "Type", "SINGLE_SELECT",
                 ["Roadmap", "Feature", "Bug", "Security", "Test", "Docs",
                  "Research", "Planning", "Chore", "Pull request"])
    ensure_field(number, "Priority", "SINGLE_SELECT",
                 ["P0", "P1", "P2", "P3", "Unassigned"])
    ensure_field(number, "Risk", "SINGLE_SELECT",
                 ["High", "Medium", "Low", "Unassessed"])
    ensure_field(number, "Owner", "TEXT")
    ensure_views(number, project_id)
    sync(number)
    print("Project URL: https://github.com/users/" + OWNER +
          "/projects/" + str(number))
    print("Customize view grouping and filters in GitHub Projects UI.")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, KeyError, ValueError) as e:
        print("Bootstrap failed:", e, file=sys.stderr)
        sys.exit(1)
