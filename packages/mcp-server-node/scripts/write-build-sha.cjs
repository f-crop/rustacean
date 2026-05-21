'use strict';
// Writes the current git SHA into dist/build-sha.txt at the end of every
// `npm run build`.  Called via the "postbuild" lifecycle hook.
//
// Resolution order:
//   1. BUILD_SHA env var (set by Docker --build-arg MCP_BUILD_SHA=...)
//   2. `git rev-parse HEAD` (works in local dev with git)
//   3. "unknown" (fallback when neither is available)
const { execSync } = require('node:child_process');
const { writeFileSync } = require('node:fs');

let sha;
if (process.env.BUILD_SHA) {
  sha = process.env.BUILD_SHA.trim();
} else {
  try {
    sha = execSync('git rev-parse HEAD', {
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim();
  } catch {
    sha = 'unknown';
  }
}

writeFileSync('dist/build-sha.txt', sha);
