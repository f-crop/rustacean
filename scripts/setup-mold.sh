#!/usr/bin/env bash
# setup-mold.sh — Install mold linker and configure user-level Cargo to use it.
#
# Run once on the mars dev machine to wire up mold for all agents.
# Safe to re-run (idempotent).  Does not touch the workspace .cargo/config.toml
# so that GitHub Actions runners are not affected.
#
# How it works:
#   1. Downloads mold to ~/.local/bin/mold
#   2. Copies mold as ld.mold into the active Rust toolchain's gcc-ld directory
#      (the path the C driver searches when passed -fuse-ld=mold)
#   3. Writes ~/.cargo/config.toml to pass -fuse-ld=mold for x86_64-linux builds

set -euo pipefail

MOLD_VERSION="2.34.1"
INSTALL_DIR="$HOME/.local/bin"
USER_CARGO_CONFIG="$HOME/.cargo/config.toml"

# ── 1. Install mold ────────────────────────────────────────────────────────────
if "$INSTALL_DIR/mold" --version &>/dev/null; then
    installed_ver=$("$INSTALL_DIR/mold" --version | awk '{print $2}')
    echo "mold already installed: $installed_ver"
else
    echo "Downloading mold $MOLD_VERSION..."
    TMP=$(mktemp -d)
    curl -fsSL \
        "https://github.com/rui314/mold/releases/download/v${MOLD_VERSION}/mold-${MOLD_VERSION}-x86_64-linux.tar.gz" \
        -o "$TMP/mold.tar.gz"
    tar -xzf "$TMP/mold.tar.gz" -C "$TMP"
    mkdir -p "$INSTALL_DIR"
    cp "$TMP/mold-${MOLD_VERSION}-x86_64-linux/bin/mold"    "$INSTALL_DIR/mold"
    cp "$TMP/mold-${MOLD_VERSION}-x86_64-linux/bin/ld.mold" "$INSTALL_DIR/ld.mold"
    chmod +x "$INSTALL_DIR/mold" "$INSTALL_DIR/ld.mold"
    rm -rf "$TMP"
    echo "mold installed to $INSTALL_DIR/mold"
fi

"$INSTALL_DIR/mold" --version

# ── 2. Place ld.mold in the active toolchain's gcc-ld directory ───────────────
# The Rust toolchain passes -B<toolchain>/lib/rustlib/<target>/bin/gcc-ld to the
# C compiler.  GCC resolves -fuse-ld=mold by looking for ld.mold in that -B dir.
TOOLCHAIN_ROOT=$(rustup which rustc | sed 's|/bin/rustc||')
GCC_LD_DIR="$TOOLCHAIN_ROOT/lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld"

if [[ -d "$GCC_LD_DIR" ]]; then
    if [[ -f "$GCC_LD_DIR/ld.mold" ]]; then
        echo "ld.mold already in toolchain gcc-ld dir."
    else
        cp "$INSTALL_DIR/mold" "$GCC_LD_DIR/ld.mold"
        echo "Placed ld.mold in $GCC_LD_DIR"
    fi
else
    echo "WARNING: gcc-ld dir not found at $GCC_LD_DIR — skipping toolchain wiring."
    echo "You may need to run this again after installing the x86_64-unknown-linux-gnu target."
fi

# ── 3. Write user-level Cargo config ──────────────────────────────────────────
mkdir -p "$(dirname "$USER_CARGO_CONFIG")"

# Only write the [target] block if it isn't already there.
if grep -q 'fuse-ld=mold\|linker.*mold' "$USER_CARGO_CONFIG" 2>/dev/null; then
    echo "Cargo user config already references mold — no changes made."
else
    cat >> "$USER_CARGO_CONFIG" <<'EOF'

# ── mold linker (written by scripts/setup-mold.sh) ────────────────────────────
# Replaces rust-lld/LLD (~1.4 GB peak RSS per link) with mold (~200 MB peak RSS).
# mold is installed at ~/.local/bin/mold; ld.mold is wired into the toolchain
# gcc-ld directory so -fuse-ld=mold resolves correctly at link time.
# Applies only to builds run as this user on this machine (not CI).
[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
EOF
    echo "Wrote mold linker config to $USER_CARGO_CONFIG"
fi

# Only write [build] jobs = 1 if it isn't already there.
# CI runners use the workspace .cargo/config.toml which deliberately omits this
# setting so parallel builds on GitHub Actions are unaffected.
if grep -q 'jobs\s*=' "$USER_CARGO_CONFIG" 2>/dev/null; then
    echo "Cargo user config already sets jobs — no changes made."
else
    cat >> "$USER_CARGO_CONFIG" <<'EOF'

# ── serialized linking (written by scripts/setup-mold.sh) ─────────────────────
# Limits each cargo invocation to one linker process.  Cross-agent serialization
# is handled by the cargo-locked flock wrapper; this setting ensures a single
# agent doesn't itself fan out N linker processes at once.
# Applies only to this user on this machine (not CI).
[build]
jobs = 1
EOF
    echo "Wrote jobs = 1 to $USER_CARGO_CONFIG"
fi

echo ""
echo "Done.  Verify with:"
echo "  cargo check -p rb-tracing 2>&1 | tail -3"
