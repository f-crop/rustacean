#!/bin/bash
set -euo pipefail

# Refuse to start without at least one authorized public key — an empty
# authorized_keys would leave no way to SSH in.
if [[ -z "${RB_SSH_AUTHORIZED_KEYS:-}" ]]; then
    echo "ERROR: RB_SSH_AUTHORIZED_KEYS is not set; refusing to start sshd without a public key." >&2
    echo "       Set RB_SSH_AUTHORIZED_KEYS to one or more newline-separated SSH public keys." >&2
    exit 1
fi

# Prepare the login user's SSH directory with strict permissions.
mkdir -p /home/loginuser/.ssh
chmod 700 /home/loginuser/.ssh
printf '%s\n' "${RB_SSH_AUTHORIZED_KEYS}" > /home/loginuser/.ssh/authorized_keys
chmod 600 /home/loginuser/.ssh/authorized_keys
chown -R loginuser:loginuser /home/loginuser/.ssh

# Ensure the shared credentials volume directory is writable by loginuser.
# Docker creates named volumes as root; loginuser needs write access for
# `claude /login` to persist .credentials.json.
chown loginuser:loginuser "${CLAUDE_CONFIG_DIR:-/home/loginuser/.claude}" 2>/dev/null || true

# Generate SSH host keys on first start (persist via named volume if desired).
ssh-keygen -A

# Launch background OAuth token-refresh loop (runs as root, writes via realpath).
# Keeps credentials.json fresh so agent-runner sessions always start with a valid
# access token without relaxing the read-only mount on the agent side.
node /usr/local/bin/token-refresh.js &

exec /usr/sbin/sshd -D -e
