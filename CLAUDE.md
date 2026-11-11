# Agent instructions

## Worktrees

Task work runs in `.worktrees/T-xxx` on a `task/T-xxx-…` branch. Each
worktree grows to 15–25 GB, almost all of it its own `target/`, so remove a
worktree as soon as its branch is merged where it belongs (usually `main`):

1. Confirm it is done with: `git -C .worktrees/T-xxx status --porcelain`
   prints nothing, no running process uses the folder, and
   `git merge-base --is-ancestor <worktree HEAD> <target branch>` succeeds.
2. `git worktree remove .worktrees/T-xxx`, then `git worktree prune`. Keep the
   branch.

Clean up merged worktrees before creating new ones.
