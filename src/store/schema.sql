-- Ferrogram local cache, migration 1. The cache never stores credentials.
CREATE TABLE IF NOT EXISTS chats (
    id              INTEGER PRIMARY KEY,
    kind            TEXT    NOT NULL,
    title           TEXT    NOT NULL,
    unread          INTEGER NOT NULL DEFAULT 0,
    muted           INTEGER NOT NULL DEFAULT 0,
    pinned          INTEGER NOT NULL DEFAULT 0,
    read_outbox     INTEGER NOT NULL DEFAULT 0,
    read_inbox      INTEGER NOT NULL DEFAULT 0,
    last_date       INTEGER NOT NULL DEFAULT 0,
    last_message_id INTEGER
);

CREATE TABLE IF NOT EXISTS messages (
    chat_id     INTEGER NOT NULL,
    id          INTEGER NOT NULL,
    sender_id   INTEGER,
    sender_name TEXT,
    outgoing    INTEGER NOT NULL,
    date        INTEGER NOT NULL,
    text        TEXT    NOT NULL,
    media       TEXT,
    reply_to    INTEGER,
    edited      INTEGER NOT NULL DEFAULT 0,
    reactions   TEXT,
    service     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (chat_id, id)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS kv (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
