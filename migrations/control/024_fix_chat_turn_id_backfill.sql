-- Control schema: forward fix for migration 023 — cast uuid→text for max() window.
-- PostgreSQL raises "function max(uuid) does not exist" when uuid columns are
-- passed directly to max() in some configurations.  This migration re-runs the
-- parent_user_id backfill with the corrected text-cast approach and then
-- re-propagates turn_id for any rows left NULL by the failed 023 run.
--
-- Idempotent: WHERE clauses scope all updates to rows that still need repair.

-- Step 1: re-stamp parent_user_id for any non-user rows still NULL.
WITH walked AS (
    SELECT id, role,
           max(CASE WHEN role = 'user' THEN id::text END)::uuid
               OVER (PARTITION BY session_id ORDER BY seq
                     ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS user_id
    FROM control.chat_messages
)
UPDATE control.chat_messages m
    SET parent_user_id = w.user_id
FROM walked w
WHERE m.id = w.id
  AND m.role != 'user'
  AND m.turn_id IS NULL
  AND m.parent_user_id IS NULL;

-- Step 2: stamp turn_id on any user rows still NULL (023 step 2 may have
-- succeeded, but we guard with the WHERE clause for idempotency).
UPDATE control.chat_messages
    SET turn_id = gen_random_uuid()
WHERE role = 'user'
  AND turn_id IS NULL;

-- Step 3: propagate turn_id from user rows to non-user rows still NULL.
UPDATE control.chat_messages m
    SET turn_id = u.turn_id
FROM control.chat_messages u
WHERE m.parent_user_id = u.id
  AND m.turn_id IS NULL;
