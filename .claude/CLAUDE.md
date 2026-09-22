# Cardigan: A bi-directional carddav sync between Apple contacts and Fastmail

## Version Control

This is a **jj (jujutsu) repo**. Never use git commands (including `git worktree`).
Use only `jj` commands for all version control operations.

## commands

- **Build**: `mise run build`
- **Format**: `mise run fmt`
- **Lint**: `mise run lint`
- **Format and Lint**: mise run fmt-lint
- **Tests**: `mise run test`
- **Review insta snapshots**: `mise run insta-review`

## Workflows

**Multi-step implementations:** Each logical step **MUST** be its own jj changeset. Before
starting a step, ensure the working copy is empty (`jj new` if needed). At the end of each
step, run the end-of-task routine below.

**After completing each task (end-of-task routine):**

These **MUST** be run as separate Bash commands. Do **NOT** join them into a single one with `&&`.

1. `mise run fmt-lint` — format code
2. `mise run test` — verify tests pass
3. `jj desc -m "type(scope): description\n\nbody"` — update working copy description

### Workspaces

Use the `jj-workspace` skill and `jj` commands to manage workspaces. _DO NOT_ use `git` commands.

When creating a new workspace, run

```bash
direnv allow
mise trust
```

To verify the baseline state, run `mise run test`.

## Testing

- Tests live alongside source code in `#[cfg(test)]` modules

## Conventions

- **Commits:** Valid scopes: match crate names
- **Error handling:** `thiserror` for all library crates; `anyhow` for binaries

## Insights

This project uses `.insights/` for research, triage docs, specs, plans, and personal notes
managed by the `insights` CLI.

**At the start of brainstorming, spec writing, or planning work**, dispatch the
`insights-locator` agent to check for prior context before proceeding. Use
`insights-analyzer` to read the most relevant documents. Use the `insights-research`
skill to orchestrate both and save a research document.

Directory layout:

- `.insights/issues/` — triage documents (CG-XX-triage-\*.md)
- `.insights/shared/specs/` — specs (CG-XX-spec-\*.md)
- `.insights/shared/plans/` — plans (CG-XX-plan-\*.md)
- `.insights/shared/research/` — research documents
- `.insights/scotte/` — personal notes
- `.insights/searchable/` — hardlink mirror for grep/search (read-only; strip "searchable/"
  from any path before reporting or editing)

All `.insights/` artifact files must include YAML front-matter.
See `.insights/shared/schema.md` for the full schema and vocabulary.
