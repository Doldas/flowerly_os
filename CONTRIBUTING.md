# Contributing

Thanks for helping build Flowerly OS. Humans and agents contribute through the
same pipeline: the same source tree, build system, tests, issue tracker, CI,
release process, and provenance.

## Ground rules

- Read [VISION.md](VISION.md), [ARCHITECTURE.md](ARCHITECTURE.md), the relevant
  records in [adr/](adr/), and the current milestone in [ROADMAP.md](ROADMAP.md)
  before designing a change.
- Keep changes scoped to the current milestone and leave the repository
  buildable.
- Add tests before claiming success.
- Do not invent parallel architecture, bypass provenance for autonomous
  actions, couple the system to one AI provider, or make boot depend on AI.

## Branches

- The default branch is `main`. There is no permanent development branch.
- Use short-lived branches named for their purpose: `feat/flow-model`,
  `fix/generation-rollback`, `docs/ambient-interaction`, `ci/rust-clippy`.

## Commits

Use conventional-style prefixes:

```text
feat:      a new capability
fix:       a bug fix
docs:      documentation only
test:      tests only
refactor:  behavior-preserving change
perf:      performance change
build:     build system or tooling
ci:        continuous integration
chore:     maintenance
```

Examples: `feat(flow): add persistent Flow metadata store`,
`fix(generation): return previous state on rollback`. A commit should represent
one coherent change.

## Development loop

1. Install [`just`](https://github.com/casey/just) and a stable Rust toolchain
   (`rustup`, which respects `rust-toolchain.toml`).
2. Create a short-lived branch.
3. Make the change and run the checks:

```sh
just check    # formatting + clippy + tests
just test     # test suite
just lint     # formatting + clippy
just build    # compile the workspace
```

4. Push and open a pull request.

## Pull requests

Open a pull request even for agent-generated changes once CI exists. A good PR
states what changed, why, which milestone it advances, and how it was tested.
Keep the description honest about what is and is not implemented.

## Architecture decisions

- Check the existing records in [adr/](adr/) before changing architecture.
- For a consequential decision, add a new ADR. Filenames follow
  `NNNN-short-title.md` (for example
  `0001-rust-as-primary-system-language.md`). Record the decision, the
  alternatives considered, the evidence, the consequences, and whether it is
  reversible.
- Use [rfcs/](rfcs/) for larger proposals that need discussion before a
  decision. An RFC that is accepted becomes an ADR plus implementation issues.

## Issue labels

Issues are labeled along a few axes:

- **Type:** `type:feature`, `type:bug`, `type:architecture`, `type:research`,
  `type:docs`, `type:test`, `type:security`.
- **Area:** `area:core`, `area:flow`, `area:session`, `area:goal`,
  `area:generation`, `area:evolution`, `area:memory`, `area:model`, `area:image`,
  `area:ui`.
- **Priority:** `priority:p0` through `priority:p3`.
- **Status:** `status:blocked`, `status:needs-design`, `status:ready`.
- **Process:** `agent-friendly`, `human-review`, `breaking`.

## Questions

Open a discussion or an issue with the `type:architecture` label. Do not add
new dependencies, languages, or infrastructure without a concrete reason and an
ADR.
