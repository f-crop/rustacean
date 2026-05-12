FROM node:20-bookworm-slim

RUN apt-get update -qq && \
    apt-get install -y --no-install-recommends \
        openssh-server \
        ca-certificates && \
    rm -rf /var/lib/apt/lists/* && \
    mkdir -p /var/run/sshd && \
    groupadd --gid 65532 loginuser && \
    useradd --uid 65532 --gid 65532 --create-home --shell /bin/bash loginuser

ARG CLAUDE_CODE_VERSION=2.1.139
RUN npm install --omit=dev --no-fund --no-audit -g \
        @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} && \
    npm cache clean --force && \
    claude --version

# Harden sshd: pubkey only, no root, no password auth.
RUN sed -i \
        -e 's/^#\?PasswordAuthentication.*/PasswordAuthentication no/' \
        -e 's/^#\?PermitRootLogin.*/PermitRootLogin no/' \
        -e 's/^#\?ChallengeResponseAuthentication.*/ChallengeResponseAuthentication no/' \
        -e 's/^#\?PubkeyAuthentication.*/PubkeyAuthentication yes/' \
        -e 's/^#\?AllowAgentForwarding.*/AllowAgentForwarding no/' \
        -e 's/^#\?AllowTcpForwarding.*/AllowTcpForwarding no/' \
        -e 's/^#\?X11Forwarding.*/X11Forwarding no/' \
        /etc/ssh/sshd_config && \
    echo 'AllowUsers loginuser' >> /etc/ssh/sshd_config

COPY docker/claude-login/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

EXPOSE 22
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
