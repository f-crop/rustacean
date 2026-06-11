-- Control schema: Wave 9 Rewrite — backfill turn_id + parent_user_id for pre-022 rows
-- (RUSAA-1977 spec gap: migration 022 was additive-only, this closes the invariant
-- that every chat_messages row has a non-null turn_id post-migration.)
--
-- Only rows where turn_id IS NULL are touched; all post-022 rows are unchanged.
-- Idempotent: safe to re-run.

-- Step 1: stamp parent_user_id on legacy non-user rows.
-- Window covers all rows in each session (not just null-turn rows) so that legacy
-- assistant rows in a mixed session correctly inherit the owning user row even
-- when that user row was inserted after 022 (and already has a turn_id).
WITH walked AS (
    SELECT id, role,
           (max(CASE WHEN role = 'user' THEN id::text END)
               OVER (PARTITION BY session_id ORDER BY seq
                     ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW))::uuid AS user_id
    FROM control.chat_messages
)
UPDATE control.chat_messages m
    SET parent_user_id = w.user_id
FROM walked w
WHERE m.id = w.id
  AND m.role != 'user'
  AND m.turn_id IS NULL
  AND m.parent_user_id IS NULL;

-- Step 2: stamp turn_id on legacy user rows (one fresh UUID per row).
UPDATE control.chat_messages
    SET turn_id = gen_random_uuid()
WHERE role = 'user'
  AND turn_id IS NULL;

-- Step 3: propagate turn_id from user rows to their legacy non-user rows.
UPDATE control.chat_messages m
    SET turn_id = u.turn_id
FROM control.chat_messages u
WHERE m.parent_user_id = u.id
  AND m.turn_id IS NULL;
