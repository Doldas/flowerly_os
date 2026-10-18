#!/usr/bin/env python3
"""Bootstrap the native Flowerly OS GitHub Project v2 board.

Requires GitHub CLI authenticated with 'project' scope:
  gh auth refresh -s project
  python3 scripts/bootstrap-github-project.py

Safe to rerun: reuses the project, fills only unset fields, and never edits issues/PRs.
"""
import argparse
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
        raise RuntimeError("gh " + " ".join(map(str, args)) +
                           " failed: " + p.stderr.strip())
    return p.stdout.strip()


def data(*args):
    return json.loads(gh(*args))


def ensure_project(number=None):
    if number is not None:
        match = data("project", "view", number, "--owner", OWNER,
                     "--format", "json")
        if match["title"] != TITLE:
            raise RuntimeError("Project " + str(number) + " is not " + TITLE)
    else:
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
    match = next((f for f in fields if f["name"].casefold() == name.casefold()), None)
    if match:
        expected = "ProjectV2SingleSelectField" if choices else "ProjectV2Field"
        if match["type"] != expected:
            raise RuntimeError("Existing field " + repr(name) + " has incompatible type")
        missing = set(choices or []) - {o["name"] for o in match.get("options", [])}
        if missing:
            raise RuntimeError("Existing field " + repr(name) + " is missing options: " +
                               ", ".join(sorted(missing)))
        return match
    args = ["project", "field-create", number, "--owner", OWNER,
            "--name", name, "--data-type", field_type]
    if choices:
        args += ["--single-select-options", ",".join(choices)]
    created = data(*args, "--format", "json")
    print("Added field:", name)
    return created


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
            "Work type": typ, "Area": area, "Priority": priority,
            "Risk": "Unassessed",
            "Sprint": "Sprint " + sprint.group(1) if sprint else "",
            "Owner": owners}


def existing_items(project_id):
    """Read existing field IDs so a retry preserves every populated value."""
    query = """query($id:ID!,$cursor:String){node(id:$id){... on ProjectV2{
      items(first:100,after:$cursor){pageInfo{hasNextPage endCursor} nodes{
        id content{... on Issue{url} ... on PullRequest{url}}
        fieldValues(first:100){nodes{
          ... on ProjectV2ItemFieldTextValue{text field{... on ProjectV2Field{id}}}
          ... on ProjectV2ItemFieldSingleSelectValue{name field{... on ProjectV2SingleSelectField{id}}}
        }}
      }}
    }}}"""
    cursor = None
    result = {}
    while True:
        args = ["api", "graphql", "-f", "query=" + query, "-f", "id=" + project_id]
        if cursor:
            args += ["-f", "cursor=" + cursor]
        page = data(*args)["data"]["node"]["items"]
        for item in page["nodes"]:
            url = (item.get("content") or {}).get("url")
            if url:
                result[url] = item
        if not page["pageInfo"]["hasNextPage"]:
            return result
        cursor = page["pageInfo"]["endCursor"]


def sync(number, project_id):
    issues = data("issue", "list", "--repo", REPO, "--state", "all",
                  "--limit", "1000", "--json",
                  "number,title,state,labels,milestone,assignees")
    prs = data("pr", "list", "--repo", REPO, "--state", "all",
               "--limit", "1000", "--json", "number,title,state,mergedAt,labels,milestone,assignees")
    schema = {f["name"].casefold(): f for f in data(
        "project", "field-list", number, "--owner", OWNER,
        "--limit", "100", "--format", "json")["fields"]}
    existing = existing_items(project_id)
    added = 0
    for is_pr, items, path in [(False, issues, "issues"), (True, prs, "pull")]:
        for item in items:
            url = "https://github.com/" + REPO + "/" + path + "/" + str(item["number"])
            project_item = existing.get(url)
            if project_item is None:
                project_item = data("project", "item-add", number, "--owner", OWNER,
                                    "--url", url, "--format", "json")
                existing[url] = project_item
                added += 1
            populated = {v["field"]["id"] for v in
                         project_item.get("fieldValues", {}).get("nodes", [])
                         if v.get("field") and
                         (v.get("text") not in (None, "") or v.get("name") not in (None, ""))}
            for key, value in fields(item, is_pr).items():
                if not value:
                    continue
                field = schema[key.casefold()]
                if field["id"] in populated:
                    continue  # Preserve human-edited and partially imported values.
                args = ["project", "item-edit", "--id", project_item["id"],
                        "--project-id", project_id, "--field-id", field["id"]]
                args += ["--text", value] if key in ("Sprint", "Owner") else [
                    "--single-select-option-id",
                    next(o["id"] for o in field["options"] if o["name"] == value)]
                gh(*args)
            print("Synced", path, item["number"])
    print("Found", len(issues), "issues and", len(prs),
          "PRs;", added, "new Project items. Populated fields were left unchanged.")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=int, help="Resume a specific existing Project number")
    parser.add_argument("--sync-only", action="store_true",
                        help="Only add missing items and fill unset fields; "
                             "do not create fields or views")
    options = parser.parse_args(argv)
    gh("auth", "status")
    number, project_id = ensure_project(options.project)
    if options.sync_only:
        sync(number, project_id)
        print("Project URL: https://github.com/users/" + OWNER +
              "/projects/" + str(number))
        return
    ensure_field(number, "Target release", "SINGLE_SELECT", RELEASES)
    ensure_field(number, "Delivery stage", "SINGLE_SELECT",
                 ["Backlog", "Ready", "In progress", "Review", "Done"])
    ensure_field(number, "Sprint", "TEXT")
    ensure_field(number, "Area", "SINGLE_SELECT",
                 ["Core", "Flow", "Session", "Storage", "Evolution", "Goal",
                  "Agent", "Image", "Docs", "CI", "Security", "Planning", "Other"])
    # Type is GitHub's reserved, built-in issue-type field, even when hidden.
    ensure_field(number, "Work type", "SINGLE_SELECT",
                 ["Roadmap", "Feature", "Bug", "Security", "Test", "Docs",
                  "Research", "Planning", "Chore", "Pull request"])
    ensure_field(number, "Priority", "SINGLE_SELECT",
                 ["P0", "P1", "P2", "P3", "Unassigned"])
    ensure_field(number, "Risk", "SINGLE_SELECT",
                 ["High", "Medium", "Low", "Unassessed"])
    ensure_field(number, "Owner", "TEXT")
    ensure_views(number, project_id)
    sync(number, project_id)
    print("Project URL: https://github.com/users/" + OWNER +
          "/projects/" + str(number))
    print("Customize view grouping and filters in GitHub Projects UI.")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, KeyError, ValueError) as e:
        print("Bootstrap failed:", e, file=sys.stderr)
        sys.exit(1)

