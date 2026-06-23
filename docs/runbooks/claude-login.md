# Claude Login Runbook

How to authenticate a Claude Max account for use by the `agent-runner` service.

## Overview

The `claude-login` sidecar runs `openssh-server` and the `claude` CLI. A human SSHs in once, runs `claude /login`, and the resulting credentials are stored in the `claude-credentials` Docker volume. The `agent-runner` service mounts that volume read-only and reads the credentials for every `claude_code` session.

## Prerequisites

- Dev stack running (`compose/dev.yml`)
- `RB_SSH_AUTHORIZED_KEYS` set in your environment or `compose/tailscale.env` to your SSH public key
- Network access to the host on port `${CLAUDE_SSH_HOST_PORT:-12222}` (default `12222`)

## Step 1 — Provision your SSH public key

Set `RB_SSH_AUTHORIZED_KEYS` in `compose/tailscale.env` (or export before `compose up`):

```bash
# tailscale.env (one key per line; multiple keys allowed)
RB_SSH_AUTHORIZED_KEYS="ssh-ed25519 AAAA... you@host"
```

Restart the `claude-login` service to pick up the key change:

```bash
docker compose -f compose/dev.yml restart claude-login
```

## Step 2 — SSH into the sidecar

```bash
ssh -p 12222 loginuser@<host>
# On mars (MagicDNS — works when your machine is on the tailnet):
ssh -p 12222 loginuser@mars
# On mars (Tailscale IP — always works from the tailnet):
ssh -p 12222 loginuser@100.87.157.74
```

Expected: you get a shell prompt. If you see `Permission denied (publickey)`, double-check that `RB_SSH_AUTHORIZED_KEYS` matches your private key.

## Step 3 — Log in to Claude

Inside the SSH session:

```bash
loginuser@claude-login:~$ claude /login
```

Follow the interactive OAuth flow: Claude opens a browser URL, paste the authorization code back into the terminal. On success you will see confirmation and a `~/.claude/credentials.json` file:

```bash
loginuser@claude-login:~$ ls ~/.claude/
credentials.json
```

Exit the SSH session (`exit` or Ctrl-D). The credentials are now stored in the `claude-credentials` volume.

## Step 4 — Verify agent-runner picks up the credentials

Start (or restart) the `agent-runner` service:

```bash
docker compose -f compose/dev.yml restart agent-runner
```

Check that it sees the credentials:

```bash
docker compose -f compose/dev.yml exec agent-runner \
    test -f /home/loginuser/.claude/credentials.json && echo "OK" || echo "MISSING"
```

Expected: `OK`.

Trigger a `claude_code` session. If credentials are absent, the runner emits:

```
claude_not_logged_in: /home/loginuser/.claude/credentials.json not found.
SSH into the application via port 12222 and run `claude /login`.
```

## Credential expiry

Claude Max refresh tokens are long-lived (weeks/months). If a session starts failing with auth errors after extended uptime, SSH in again and re-run `claude /login` to refresh.

## Rollback

To disable the SSH login path, stop the sidecar:

```bash
docker compose -f compose/dev.yml stop claude-login
```

Existing credentials remain in the `claude-credentials` volume. To wipe them:

```bash
docker volume rm rustacean_claude-credentials
```

`claude_code` sessions will fail with `claude_not_logged_in` until the next login.
