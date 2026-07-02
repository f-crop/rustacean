> **Title format**: `[REQ-XX-NN] type(scope): description` — CI will reject PRs without the bracket prefix.

## Summary

<!-- What does this PR do and why? -->

## Test Plan

- [ ] All CI jobs pass
- [ ] Manual testing performed (describe below)

<!-- Describe manual verification steps -->

## Checklist

- [ ] PR links a GitHub issue via `Closes #XX`
- [ ] No internal board tracking references in title, body, or commit messages
- [ ] Tests added or updated for changed behaviour
- [ ] Docs updated if API or architecture changed

## Baseline Regeneration (complete only if `baseline.json` changed)

- [ ] `baseline.json` update is intentional — a metric drop was accepted after manual review
- [ ] `results-snapshot.json` was regenerated via the `regenerate-eval-baseline` workflow
- [ ] New baseline metrics have been reviewed and approved by a second engineer
