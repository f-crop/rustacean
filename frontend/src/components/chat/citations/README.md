# Chat Citations

Renders `CitationV1` chips inline in chat panel assistant messages.

## Contract version

**v1** — frozen by ADR-014 §5. Any breaking change to the envelope requires an ADR amendment and a `version` bump. S4 renders only `version: "v1"` items; any other version surfaces a soft warning badge.

## `CitationV1` fields

| Field | Type | Description |
|---|---|---|
| `version` | `"v1"` | Envelope version tag |
| `repo_id` | UUID string | Repository this symbol belongs to |
| `file_path` | string | Relative path within the repo |
| `line_range.start` | number | First line (1-indexed) |
| `line_range.end` | number | Last line (inclusive) |
| `commit_sha` | string | SHA at ingest time; used as stable blob anchor |
| `score` | number [0,1] | Fused retrieval score (dense + sparse RRF) |
| `source_kind` | `"dense" \| "sparse" \| "hybrid" \| "rerank"` | Which retrieval leg(s) produced this hit |

## `source_kind` badges

| Kind | Badge | Color |
|---|---|---|
| `dense` | D | Blue |
| `sparse` | S | Green |
| `hybrid` | H | Purple |
| `rerank` | R | Gold |

## Usage

```tsx
import { CitationChip } from "@/components/chat/citations";

<CitationChip
  citation={citationV1Object}
  repoFullName="f-crop/rustacean"   // optional; enables GitHub link
/>
```

When `repoFullName` is provided, the chip renders as an `<a>` that opens:
`https://github.com/{repoFullName}/blob/{commit_sha}/{file_path}#L{start}-L{end}`

Without `repoFullName` the chip is a non-interactive `<span>` showing the same text.

## v1-freeze rule

Do not modify the CitationV1 TypeScript interface without a matching ADR-014 amendment. The interface intentionally mirrors the Rust struct byte-for-byte (after JSON serialisation). Use the `version` field to gate future format changes gracefully.
