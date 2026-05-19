# NPM Token Rotation Runbook

How to rotate the `NPM_TOKEN` GitHub Actions secret used by `mcp-server-publish.yml` to publish packages under the `@rustbrain` npm org.

Rotate every **90 days** or immediately upon suspected compromise.

## Overview

The `@rustbrain` npm org owns the `@rustbrain/mcp-server` package. CI publishes via an automation-type token stored as the `NPM_TOKEN` GitHub Actions secret on `f-crop/rustacean`. Automation tokens bypass 2FA, so they must be kept short-lived and scoped to `@rustbrain` only.

## Prerequisites

- Owner access to the team's npm account (npmjs.com)
- Admin access to `f-crop/rustacean` on GitHub (to write Actions secrets)
- `gh` CLI authenticated as a user with repo admin on `f-crop/rustacean`

## Step 1 — Generate a new automation token on npmjs.com

1. Log in at <https://www.npmjs.com> as the team account.
2. Navigate to: **Profile menu → Access Tokens → Generate New Token → Classic Token**.
3. Set **Token type** to **Automation** (bypasses 2FA; required for CI).
4. Set the token description to: `rustacean-ci-<YYYY-MM-DD>` (e.g. `rustacean-ci-2026-05-19`).
5. Under **Packages and scopes**, restrict to:
   - **Organization**: `@rustbrain`
   - **Permissions**: `Read and write` (publish)
6. Click **Generate Token** and copy the value immediately — it is shown only once.

## Step 2 — Set the GitHub Actions secret

Run this command locally (requires `gh` CLI with repo admin):

```bash
gh secret set NPM_TOKEN \
  --repo f-crop/rustacean \
  --body "<paste-token-here>"
```

Verify it appears in the secret list (value is always redacted):

```bash
gh secret list --repo f-crop/rustacean | grep NPM_TOKEN
```

Expected output: a line with `NPM_TOKEN` and today's date.

## Step 3 — Revoke the old token

1. Return to <https://www.npmjs.com> → **Profile menu → Access Tokens**.
2. Find the previous `rustacean-ci-*` token (sorted by creation date).
3. Click **Revoke**.

Revoking the old token before confirming CI succeeds with the new one can break the publish job. Confirm a successful CI run first (see Verification below), then revoke.

## Verification

Trigger the `mcp-server-publish.yml` workflow to confirm the new token works:

```bash
# Dry-run publish via workflow_dispatch (if the workflow supports it)
gh workflow run mcp-server-publish.yml --repo f-crop/rustacean

# Or check the most recent run result
gh run list --repo f-crop/rustacean --workflow mcp-server-publish.yml --limit 5
```

If the publish job completes without `403 Forbidden` or `E401` errors, the new token is valid. Then revoke the old one (Step 3).

## Rotation schedule

| Event | Action |
|-------|--------|
| Every 90 days | Run this runbook |
| Suspected compromise | Immediately revoke old token (Step 3), then run this runbook |
| Team npm account change | Regenerate under the new owner account |

Set a calendar reminder for the next rotation date when you finish: **90 days from `rustacean-ci-<today>` creation**.

## Initial setup (first-time only)

The first-time setup requires reserving the `@rustbrain` org before generating any tokens:

1. Log in at <https://www.npmjs.com> as the team account.
2. Click **+** → **Create Organization**.
3. Enter org name: `rustbrain` (this claims `@rustbrain`).
4. Select the **Free** plan (public packages only).
5. Confirm. The org is now `@rustbrain`.
6. Proceed from Step 1 above to generate the first token.

## References

- Consuming workflow: `.github/workflows/mcp-server-publish.yml`
- Published package: `@rustbrain/mcp-server` on npmjs.com
- Parent issue: RUSAA-1539 (MCP server package + publish workflow)
- npm automation token docs: <https://docs.npmjs.com/creating-and-viewing-access-tokens>
