-- Wave 6: store inline item source text for ≤ 512 KiB items.
-- The parse-worker slices line ranges from source files; this column
-- lets the item-lookup endpoint return source_preview without a blob fetch.
ALTER TABLE code_symbols ADD COLUMN IF NOT EXISTS source_text TEXT;
