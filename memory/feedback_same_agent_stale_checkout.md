---
name: Same-agent stale checkout blocks all writes
description: When an issue has a stale checkout from a prior run of the same agent, even POST /comments and PATCH fail with run ownership conflict — broad expectedStatuses does not help
type: feedback
---

When RUSAA-668 had `checkoutRunId=b0b03c05` (prior run) and current run was `d900de7a`, even with ALL valid expectedStatuses, checkout returned conflict. POST /comments also returned ownership conflict.

**Why:** The Paperclip server checks that the CURRENT run ID matches the checkout run ID for all write operations (PATCH, POST /comments, release). Same-agent-different-run is treated like a conflict.

**How to apply:** If a task has a stale checkout from a prior run:
1. You CANNOT write to it from a new run — not even comments.
2. Escalate to CTO/manager via a comment on the parent/sibling issue.
3. Or wait for the stale run to expire and the checkout to be cleared.
4. "Broad expectedStatuses" in POST /checkout only helps when the issue status mismatch causes the conflict — NOT when the run ID conflicts.
