-- Migration 5: whether the other side of a private chat is a contact (chat
-- folders can include "Contacts" / "Non-contacts"). Folders themselves are
-- cached as JSON in `kv` ("folders").
ALTER TABLE chats ADD COLUMN contact INTEGER NOT NULL DEFAULT 0;
