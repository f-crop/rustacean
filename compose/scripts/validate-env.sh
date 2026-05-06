#!/usr/bin/env bash
# validate-env.sh — validates an env file against compose/env.schema.toml
#
# Usage:
#   compose/scripts/validate-env.sh <env-file> [--service <svc>]
#
# Arguments:
#   <env-file>         Path to env file (e.g. compose/tailscale.env, compose/dev.env)
#   --service <svc>    Only check vars that serve this service (optional)
#
# Exit codes:
#   0   All checks passed
#   1   One or more vars missing or failed regex validation
#   2   Usage error (wrong arguments, schema not found)
#
# Requires: python3 (3.11+ for tomllib; 3.9–3.10 use tomli if installed)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SCHEMA_FILE="$REPO_ROOT/compose/env.schema.toml"

# -- Args --------------------------------------------------------------------

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <env-file> [--service <service-name>]" >&2
  exit 2
fi

ENV_FILE="$1"
shift

FILTER_SERVICE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --service) FILTER_SERVICE="$2"; shift 2 ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ ! -f "$ENV_FILE" ]]; then
  echo "[validate-env] ERROR: env file not found: $ENV_FILE" >&2
  exit 2
fi

if [[ ! -f "$SCHEMA_FILE" ]]; then
  echo "[validate-env] ERROR: schema not found: $SCHEMA_FILE" >&2
  exit 2
fi

# -- Delegate to Python ------------------------------------------------------

python3 - "$ENV_FILE" "$SCHEMA_FILE" "$FILTER_SERVICE" <<'PYEOF'
import sys
import os
import re

env_file   = sys.argv[1]
schema_file = sys.argv[2]
filter_svc  = sys.argv[3]  # empty string = no filter

# -- Load TOML (3.11+ built-in; fall back to tomli) --------------------------
try:
    import tomllib
    def load_toml(path):
        with open(path, "rb") as f:
            return tomllib.load(f)
except ImportError:
    try:
        import tomli as tomllib
        def load_toml(path):
            with open(path, "rb") as f:
                return tomllib.load(f)
    except ImportError:
        # Last resort: minimal hand-rolled parser for our schema subset
        def load_toml(path):
            return _parse_toml_minimal(path)

def _parse_toml_minimal(path):
    """Minimal TOML parser for our schema structure only."""
    result = {"var": {}}
    current_section = None
    with open(path, encoding="utf-8") as f:
        for raw_line in f:
            line = raw_line.strip()
            if not line or line.startswith("#"):
                continue
            # Section header: [var.KEY]
            m = re.match(r'^\[var\.([^\]]+)\]$', line)
            if m:
                current_section = m.group(1)
                result["var"][current_section] = {}
                continue
            # Top-level key
            if current_section is None:
                m = re.match(r'^(\w+)\s*=\s*(.+)$', line)
                if m:
                    result[m.group(1)] = _toml_value(m.group(2))
                continue
            # Section key = value
            m = re.match(r'^(\w+)\s*=\s*(.+)$', line)
            if m:
                result["var"][current_section][m.group(1)] = _toml_value(m.group(2))
    return result

def _toml_value(raw):
    raw = raw.strip()
    if raw.startswith('"'):
        return raw.strip('"')
    if raw.startswith("'"):
        return raw.strip("'")
    if raw == "true":
        return True
    if raw == "false":
        return False
    if raw.startswith("["):
        items = raw.strip("[]").split(",")
        return [i.strip().strip('"').strip("'") for i in items if i.strip()]
    return raw

# -- Parse env file ----------------------------------------------------------

def load_env_file(path):
    """Returns dict of key→value from a docker-compose style env file."""
    env = {}
    with open(path, encoding="utf-8") as f:
        for raw_line in f:
            line = raw_line.strip()
            if not line or line.startswith("#"):
                continue
            if "=" not in line:
                continue
            key, _, val = line.partition("=")
            env[key.strip()] = val.strip()
    return env

# -- Main validation ---------------------------------------------------------

schema = load_toml(schema_file)
env    = load_env_file(env_file)

vars_schema = schema.get("var", {})
errors       = []
warnings     = []

for var_name, spec in vars_schema.items():
    services = spec.get("services", [])

    # Skip _compose-only vars when filtering by a specific service
    if filter_svc:
        if not any(s == filter_svc for s in services):
            continue

    required = spec.get("required", False)
    regex    = spec.get("regex", "")
    danger   = spec.get("danger", "")
    description = spec.get("description", "")

    val = env.get(var_name, "")
    present = bool(val)

    if required and not present:
        msg = f"  MISSING (required): {var_name}"
        if description:
            msg += f"\n    → {description}"
        if danger:
            msg += f"\n    ⚠ {danger}"
        errors.append(msg)
        continue

    if present and regex:
        try:
            if not re.match(regex, val):
                msg = f"  INVALID format: {var_name}={val!r}"
                msg += f"\n    → expected pattern: {regex}"
                if danger:
                    msg += f"\n    ⚠ {danger}"
                errors.append(msg)
        except re.error as exc:
            warnings.append(f"  BAD regex in schema for {var_name}: {exc}")

# -- Report ------------------------------------------------------------------

label = f"[validate-env] {os.path.basename(env_file)}"
if filter_svc:
    label += f" (service={filter_svc})"

if not errors and not warnings:
    print(f"{label}: OK — all checks passed ({len(vars_schema)} vars in schema)")
    sys.exit(0)

if warnings:
    for w in warnings:
        print(f"{label}: WARNING\n{w}", file=sys.stderr)

if errors:
    print(f"{label}: FAILED — {len(errors)} error(s):", file=sys.stderr)
    for e in errors:
        print(e, file=sys.stderr)
    sys.exit(1)

sys.exit(0)
PYEOF
