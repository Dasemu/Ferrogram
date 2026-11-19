-- Migration 3: profile photo id per chat (the avatar bytes live in the
-- on-disk avatar cache, keyed by chat id + photo id).
ALTER TABLE chats ADD COLUMN photo_id INTEGER;
