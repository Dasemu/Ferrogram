-- Migration 2: history contiguity. For each chat, [lo, hi] is a range of
-- message ids that the *network* confirmed to be complete in the cache
-- (lo = 0 means "down to the very first message"). Cached messages outside
-- the range may have gaps and are never served as a full page.
CREATE TABLE IF NOT EXISTS history_ranges (
    chat_id INTEGER PRIMARY KEY,
    lo      INTEGER NOT NULL,
    hi      INTEGER NOT NULL
);
