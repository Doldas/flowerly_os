#!/usr/bin/env bash
# Flowerly OS — deterministic GitHub project setup.
#
# Creates (idempotently) the labels, milestones, project board and initial
# roadmap issues defined by the repository specification, using the GitHub
# CLI. Requires an authenticated `gh` with `repo`, `project`, and `workflow`
# scopes. Dry-run with DRY_RUN=1; override the repo with REPO=owner/name.
#
#   usage: ./scripts/bootstrap-github.sh

set -euo pipefail

REPO="${REPO:-Doldas/flowerly_os}"
DRY_RUN="${DRY_RUN:-0}"

if ! command -v gh >/dev/null 2>&1; then
    echo "error: gh CLI not found — install it or run the manual checklist below." >&2
    exit 1
fi

gh repo view "$REPO" >/dev/null 2>&1 || {
    echo "error: cannot access $REPO — is gh authenticated and the repo public?" >&2
    exit 1
}

run() {
    if [[ "$DRY_RUN" == "1" ]]; then
        echo "[dry-run] $*"
    else
        "$@"
    fi
}

api() {
    # gh api returns exit code 0 even for 404s via `-f`, so pass URL carefully.
    run gh api "$@"
}

label() {
    local name="$1" color="$2" desc="$3"
    if gh label view "$name" --repo "$REPO" >/dev/null 2>&1; then
        echo "label exists: $name"
        return
    fi
    run gh label create "$name" --repo "$REPO" --color "$color" --description "$desc"
}

milestone() {
    local title="$1"
    if gh api "repos/$REPO/milestones" -q ".[] | select(.title == \"$title\") | .title" \
        | grep -q "$title"; then
        echo "milestone exists: $title"
        return
    fi
    if [[ "$DRY_RUN" == "1" ]]; then
        echo "[dry-run] gh api repos/$REPO/milestones -f title='$title'"
        return
    fi
    gh api "repos/$REPO/milestones" -f title="$title" >/dev/null
    echo "created milestone: $title"
}

issue() {
    local title="$1" labels="$2" milestone="$3" body="$4"
    if gh issue list --repo "$REPO" --label "type:feature" --search "title:$title in:title" \
        -q ".[] | select(.title == \"$title\")" 2>/dev/null | grep -q "$title"; then
        echo "issue exists: $title"
        return
    fi
    local args=(-R "$REPO" --title "$title" --label "$labels" --body "$body")
    if [[ -n "$milestone" ]]; then
        args+=(--milestone "$milestone")
    fi
    if [[ "$DRY_RUN" == "1" ]]; then
        echo "[dry-run] gh issue create ${args[*]}"
        return
    fi
    gh issue create "${args[@]}" >/dev/null
    echo "created issue: $title"
}

project_v2() {
    if [[ "$DRY_RUN" == "1" ]]; then
        echo "[dry-run] project_v2"
        return
    fi
    local owner_id
    owner_id="$(gh api graphql -f query='
        query { user(login: "Doldas") { id } }' -q .data.user.id)"
    local existing
    existing="$(gh api graphql -f query='
        query($e: ID!) {
          user(login: "Doldas") {
            projectsV2(first: 50) { nodes { id title } }
          }
        }' 2>/dev/null | sed -n 's/.*"title": "Flowerly OS.*//p')"
    if [[ -z "$existing" ]]; then
        gh api graphql -f query='
            mutation($o: ID!, $t: String!) {
              createProjectV2(input: { ownerId: $o, title: $t }) {
                projectV2 { id title }
              }
            }' -f o="$owner_id" -f t="Flowerly OS" >/dev/null
        echo "created project board: Flowerly OS"
    else
        echo "project board exists"
    fi
}

echo "Configuring $REPO"

# --- Labels (spec section 25) ------------------------------------------------
label "type:feature"      "a2eeef" "New capability"
label "type:bug"          "d73a4a" "Defect"
label "type:architecture" "c5def5" "Architecture / design work"
label "type:research"     "fef2c0" "Research spike"
label "type:docs"         "0075ca" "Documentation"
label "type:test"         "c2e0c6" "Tests / verification"
label "type:security"     "b60205" "Security"
label "area:core"         "5319e7" "Core runtime"
label "area:flow"         "5319e7" "Flow subsystem"
label "area:session"      "5319e7" "Session subsystem"
label "area:goal"         "5319e7" "Goal subsystem"
label "area:generation"   "5319e7" "Generation subsystem"
label "area:evolution"    "5319e7" "Evolution World / evolutiond"
label "area:memory"       "5319e7" "User/agent memory"
label "area:model"        "5319e7" "Model invocation / providers"
label "area:image"        "5319e7" "Image / boot"
label "area:ui"           "5319e7" "Shell / UI"
label "priority:p0"       "b60205" "Blocking release"
label "priority:p1"       "d93f0b" "High priority"
label "priority:p2"       "fef2c0" "Medium priority"
label "priority:p3"       "0e8a16" "Low priority"
label "status:blocked"    "1d76db" "Blocked by something else"
label "status:needs-design" "fbca04" "Needs design before implementation"
label "status:ready"      "0e8a16" "Ready for implementation"
label "agent-friendly"    "c5def5" "Well-suited for autonomous agents"
label "human-review"      "5319e7" "Requires human review"
label "breaking"          "b60205" "Breaking change"

# --- Milestones (spec section 26) --------------------------------------------
for m in \
    "0.0.1 Repository Foundation" \
    "0.0.2 Core Runtime Skeleton" \
    "0.0.3 Persistent State" \
    "0.0.4 Flows and Sessions" \
    "0.0.5 Generations" \
    "0.0.6 Evolution Lab" \
    "0.0.7 Goal Runtime" \
    "0.0.8 Agent Runtime" \
    "0.0.9 Minimal Agentic Environment" \
    "0.1.0 First Real Agentic OS"; do
    milestone "$m"
done

# --- Project board (Projects v2) ---------------------------------------------
project_v2

# --- Initial roadmap issues --------------------------------------------------
issue "Sprint 1: integration test harness for daemon health/status over IPC" \
    "type:test,agent-friendly,priority:p1" "0.0.2 Core Runtime Skeleton" \
    "Start every service (flowd, goald, sessiond, ...), assert Ping/Status over the Unix socket, and prove a clean shutdown. See the Sprint 1 prompt in docs/AGENTIC_OS_REPOSITORY_ARCHITECTURE_SPRINTS_AND_OPENCODE.md."

issue "Sprint 3: persistent PTY sessions assigned to Flows" \
    "type:feature,area:session,area:flow,priority:p0,human-review" "0.0.4 Flows and Sessions" \
    "General Flow auto-exists; users activate Flows; PTY shells persist and reattach after terminal detach (vertical slice in the Sprint 3 prompt)."

issue "Sprint 5: first Evolution World vertical slice (just vm-test)" \
    "type:feature,area:evolution,priority:p1" "0.0.6 Evolution Lab" \
    "source tree → candidate build → manifest → QEMU/KVM boot → health check → integration test → shutdown → artifacts → experiment result, with provenance. No automatic promotion."

issue "Sprint 7: bounded agent runtime (first autonomous actions)" \
    "type:feature,area:core,area:goal,priority:p1,human-review" "0.0.8 Agent Runtime" \
    "Agents created from goals, bound by capabilities, supervised by agenticd, recording executions and provenance. Providers remain replaceable."

echo "Bootstrap complete for $REPO."