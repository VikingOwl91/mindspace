# Agent rules

## Git worktrees

- Use a worktree only for an implementation task. Never for design work, releases,
  `main-*` checkouts or backports, unless you were told to explicitly.
- At most 3 worktrees at the same time.
- Always place them under `~/git/projects/mindspace-wt/<short-name>`, never under `/tmp`.
- Ask before every `git worktree add`.
- After the merge, remove the worktree right away with `git worktree remove <path>`
  (no `--force`), then run `git worktree prune`.
- Do not leave build output behind. A `target/` directory per worktree costs 10 GB or more.

## Branches and tags

- Delete a branch only with `git branch -d`, and only when it is merged into `origin/main`.
- Keep unmerged work as an `archive/<name>` tag or branch instead of deleting it.
- Never push without explicit approval.
