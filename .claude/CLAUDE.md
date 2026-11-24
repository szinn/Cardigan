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

**Multi-step implementations:** Make each logical step its own jj changeset. Before
starting a step, ensure the working copy is empty (`jj new` if needed). At the end of each
step, run the end-of-task routine below.

**After completing each task (end-of-task routine):**

Run these as three separate Bash commands, not joined with `&&`.

1. `mise run fmt-lint` — format and lint
2. `mise run test` — verify tests pass
3. `jj desc -m "…"` — update the working copy description: `type(scope): description`, a blank
   line, then the body. Put real newlines inside the quotes; `\n` in double quotes reaches jj as a
   literal backslash-n.

**Integration:** When work is finished, leave the jj changes in place. Never merge, push, move
bookmarks or open PRs; the user integrates. Skip any "how to integrate" menu and just report the
change ids and descriptions.

### Workspaces

Use the `jj-workspace` skill and `jj` commands to manage workspaces.

When creating a new workspace, run

```bash
direnv allow
mise trust
```

To verify the baseline state, run `mise run test`.

## Testing

- Unit tests live alongside source code in `#[cfg(test)]` modules
- Radicale integration tests live in `crates/integration-tests`; they are `#[ignore]`d, need
  docker/colima, and run with `mise run integration-tests`, not `mise run test`

## Running against real servers

- Credentials come from `config.sops.env` via `.envrc`; run the binary with
  `direnv exec . cargo run -- <command>` (e.g. `dump icloud`, `dry-run`).
- The state database is `Data/cardigan.db`. `CARDIGAN_DATABASE_PATH` names its directory, which
  must already exist. Copy the database before inspecting with `sqlite3`; the live file may be
  locked.
- No other client may edit both accounts. A device with both iCloud and Fastmail in Contacts
  merges cards by name, not by Cardigan's UID pairs, so one delete there can remove cards from
  two different pairs, and Cardigan then propagates both. When deletes look wrong, first ask
  which devices edit which accounts.

## Environment gotchas

- `sed` is GNU sed: use `sed -i 's/…/…/' file`, not the BSD `sed -i '' …` form.
- `.insights/issues` and `.insights/shared` are symlinks into
  `~/Development/Projects/Insights/`. If a write through the symlink is refused, write to the
  resolved path and record the project-relative `.insights/…` path in IssueBoss artifacts.

## Conventions

- **Commits:** Use the crate name as scope for crate changes; otherwise use an area scope already
  in the history, such as `deps`, `release`, `ci`, `ignore` or `claude`
- **Error handling:** `thiserror` for all library crates; `anyhow` for binaries

## Insights

This project uses `.insights/` for research, triage docs, specs, plans, and personal notes
managed by the `insights` CLI.

At the start of brainstorming, spec writing, or planning work, check for prior context with the
`issueboss:insights-research` skill (it runs `insights-locator` and `insights-analyzer`).

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
