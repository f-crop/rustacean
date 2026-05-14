#!/usr/bin/env bash
# cargo-locked.sh — Serialize cargo invocations across all Rustacean agent heartbeats.
#
# Problem: multiple Paperclip agent heartbeats run concurrently; each calls
# `cargo test/build/check`.  Even with CARGO_BUILD_JOBS=1, two simultaneous
# cargo invocations both spin up codegen+link jobs → combined RSS exceeds the
# Paperclip Node server memory ceiling.
#
# Solution: flock on /tmp/rusaa-cargo.lock so that only one cargo invocation
# runs at a time company-wide on the mars machine.
#
# Usage (install once):
#   sudo install -m 755 scripts/cargo-locked.sh /usr/local/bin/cargo-locked
# Or for non-root install:
#   install -m 755 scripts/cargo-locked.sh ~/.local/bin/cargo-locked
#
# Then agents replace:   cargo test --workspace
#              with:     cargo-locked test --workspace
#
# The lock is advisory; direct `cargo` calls (e.g. from other tools) still run
# without waiting.  Agents that load this script via their heartbeat env will
# serialize correctly.

LOCK_FILE="${RUSAA_CARGO_LOCK_FILE:-/tmp/rusaa-cargo.lock}"

exec flock --exclusive --wait 300 "$LOCK_FILE" cargo "$@"
