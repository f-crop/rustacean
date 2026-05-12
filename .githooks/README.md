# Repo-tracked git hooks

These hooks are checked into source control and shared by every contributor.
Git does not enable them automatically — run the installer once after cloning:

```bash
scripts/install-git-hooks.sh
```

That points `core.hooksPath` at this directory. Verify with:

```bash
scripts/install-git-hooks.sh --check
```

## Active hooks

| Hook | Fires on | What it does |
|------|----------|--------------|
| `post-merge` | `git pull`, `git merge` (and the merge step of `git pull --rebase`) on `main` | Backgrounds `scripts/dev-stack-auto-rebuild.sh` with `ORIG_HEAD → HEAD`. Skips when current branch is not `main`, or when `RB_SKIP_AUTO_REBUILD=1`. |

## Bypass

For batched ops (e.g. cherry-pick chains) where you don't want a rebuild after every pull:

```bash
export RB_SKIP_AUTO_REBUILD=1
git pull origin main
unset RB_SKIP_AUTO_REBUILD
```

To skip one rebuild from inside the script (after the hook has already fired):

```bash
touch compose/.no-auto-rebuild
# The rebuild script consumes and deletes this file on the next run.
```

## Logs

Background rebuilds append to `$HOME/.local/state/rustbrain/post-merge-rebuild.log`
and the structured NDJSON log at `$HOME/.local/state/rustbrain/dev-stack-rebuilds.ndjson`.

```bash
tail -f $HOME/.local/state/rustbrain/post-merge-rebuild.log
scripts/dev-stack-auto-rebuild.sh --logs 10
```
