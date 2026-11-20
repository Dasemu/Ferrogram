-- Migration 6: each dialog's own notification settings (peerNotifySettings).
-- NULL = "not set": the account default of the chat's kind applies. The
-- defaults themselves are cached as JSON in `kv` ("notify_defaults"). The
-- old `muted` column stays but is no longer read: muting is derived.
ALTER TABLE chats ADD COLUMN notify_mute_until INTEGER;
ALTER TABLE chats ADD COLUMN notify_silent INTEGER;
ALTER TABLE chats ADD COLUMN notify_previews INTEGER;
-- Chats cached before this migration keep their old mute (until forever).
UPDATE chats SET notify_mute_until = 2147483647 WHERE muted != 0;
