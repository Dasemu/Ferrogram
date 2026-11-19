-- Migration 4: per-chat draft (synced with the server). Like cached message
-- text it is plaintext in this owner-only file.
ALTER TABLE chats ADD COLUMN draft_text TEXT;
ALTER TABLE chats ADD COLUMN draft_reply INTEGER;
ALTER TABLE chats ADD COLUMN draft_date INTEGER;
