-- RiverThink schema v1. Timestamps are unix seconds (UTC).
-- Integrity is enforced here, not only in application code.

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE users (
    id             INTEGER PRIMARY KEY,
    username       TEXT    NOT NULL COLLATE NOCASE UNIQUE
                           CHECK (length(username) BETWEEN 3 AND 24),
    password_hash  TEXT    NOT NULL,
    channel        TEXT    NOT NULL CHECK (channel IN ('E','Z','D')),
    role           TEXT    NOT NULL DEFAULT 'user'   CHECK (role   IN ('user','admin')),
    status         TEXT    NOT NULL DEFAULT 'active' CHECK (status IN ('active','restricted')),
    created_at     INTEGER NOT NULL,
    last_active_at INTEGER NOT NULL
);
CREATE INDEX users_channel_active ON users (channel, status, last_active_at);

-- A user's home channel is fixed at signup (docs/DECISIONS.md D-04).
CREATE TRIGGER users_channel_immutable BEFORE UPDATE OF channel ON users
WHEN NEW.channel != OLD.channel
BEGIN SELECT RAISE(ABORT, 'user channel is immutable'); END;

CREATE TABLE sessions (
    token_hash TEXT    PRIMARY KEY,           -- sha256 hex of the cookie token
    user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX sessions_user    ON sessions (user_id);
CREATE INDEX sessions_expires ON sessions (expires_at);

-- Generation N exists once its Final Nine has been finalized.
CREATE TABLE generations (
    number    INTEGER PRIMARY KEY CHECK (number >= 1),
    starts_at INTEGER NOT NULL,
    ends_at   INTEGER NOT NULL CHECK (ends_at > starts_at)
);

CREATE TABLE ideologies (
    id                 INTEGER PRIMARY KEY,
    name               TEXT    NOT NULL COLLATE NOCASE UNIQUE
                               CHECK (length(name) BETWEEN 3 AND 40),
    creator_id         INTEGER NOT NULL REFERENCES users (id),
    description        TEXT    NOT NULL CHECK (length(description) BETWEEN 10 AND 2000),
    channel            TEXT    NOT NULL CHECK (channel IN ('E','Z','D')),
    founded_generation INTEGER NOT NULL CHECK (founded_generation >= 1),
    status             TEXT    NOT NULL DEFAULT 'active' CHECK (status IN ('active','restricted')),
    created_at         INTEGER NOT NULL,
    UNIQUE (id, channel)
);
CREATE INDEX ideologies_channel ON ideologies (channel, status);
CREATE INDEX ideologies_founding ON ideologies (channel, founded_generation);
CREATE INDEX ideologies_creator ON ideologies (creator_id, founded_generation);

CREATE TRIGGER ideologies_channel_immutable BEFORE UPDATE OF channel, founded_generation ON ideologies
WHEN NEW.channel != OLD.channel OR NEW.founded_generation != OLD.founded_generation
BEGIN SELECT RAISE(ABORT, 'ideology channel and founding generation are immutable'); END;

-- One row per user  =>  one account, one active vote. Concurrent writers cannot
-- create a second row because user_id is the primary key.
CREATE TABLE votes (
    user_id     INTEGER PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    ideology_id INTEGER NOT NULL,
    channel     TEXT    NOT NULL,
    cast_at     INTEGER NOT NULL,             -- last cast/move, drives the cooldown
    FOREIGN KEY (ideology_id, channel) REFERENCES ideologies (id, channel)
);
CREATE INDEX votes_ideology ON votes (ideology_id);

CREATE TRIGGER votes_channel_matches_user_ins BEFORE INSERT ON votes
WHEN NEW.channel != (SELECT channel FROM users WHERE id = NEW.user_id)
BEGIN SELECT RAISE(ABORT, 'vote channel must equal the voter channel'); END;

CREATE TRIGGER votes_channel_matches_user_upd BEFORE UPDATE ON votes
WHEN NEW.channel != (SELECT channel FROM users WHERE id = NEW.user_id)
BEGIN SELECT RAISE(ABORT, 'vote channel must equal the voter channel'); END;

-- Full history of support changes (current support lives in `votes`).
CREATE TABLE vote_log (
    id              INTEGER PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    from_ideology_id INTEGER REFERENCES ideologies (id),
    to_ideology_id  INTEGER NOT NULL REFERENCES ideologies (id),
    at              INTEGER NOT NULL
);
CREATE INDEX vote_log_user ON vote_log (user_id, at);

CREATE TABLE notes (
    id         INTEGER PRIMARY KEY,
    author_id  INTEGER NOT NULL REFERENCES users (id),
    title      TEXT    NOT NULL CHECK (length(title) BETWEEN 1 AND 120),
    body       TEXT    NOT NULL CHECK (length(body) <= 2000),
    status     TEXT    NOT NULL DEFAULT 'visible' CHECK (status IN ('visible','hidden')),
    created_at INTEGER NOT NULL
);
CREATE INDEX notes_author ON notes (author_id, created_at);

CREATE TABLE note_tags (
    note_id INTEGER NOT NULL REFERENCES notes (id) ON DELETE CASCADE,
    tag     TEXT    NOT NULL CHECK (length(tag) BETWEEN 1 AND 24),
    PRIMARY KEY (note_id, tag)
) WITHOUT ROWID;
CREATE INDEX note_tags_tag ON note_tags (tag, note_id);

-- ---- Elections and the Final Nine ------------------------------------------

CREATE TABLE elections (
    id                INTEGER PRIMARY KEY,
    target_generation INTEGER NOT NULL UNIQUE CHECK (target_generation >= 1),
    starts_at         INTEGER NOT NULL,
    ends_at           INTEGER NOT NULL CHECK (ends_at > starts_at),
    stage             INTEGER NOT NULL DEFAULT 0 CHECK (stage BETWEEN 0 AND 4),
    status            TEXT    NOT NULL DEFAULT 'running' CHECK (status IN ('running','finalized'))
);

-- Candidate field, frozen when the election starts (no retroactive changes).
CREATE TABLE election_entries (
    election_id      INTEGER NOT NULL REFERENCES elections (id),
    ideology_id      INTEGER NOT NULL REFERENCES ideologies (id),
    channel          TEXT    NOT NULL,
    was_incumbent    INTEGER NOT NULL DEFAULT 0,
    eliminated_stage INTEGER CHECK (eliminated_stage BETWEEN 1 AND 4),
    PRIMARY KEY (election_id, ideology_id)
) WITHOUT ROWID;

-- Audit trail of every weekly selection.
CREATE TABLE election_stage_results (
    election_id INTEGER NOT NULL REFERENCES elections (id),
    stage       INTEGER NOT NULL CHECK (stage BETWEEN 1 AND 4),
    ideology_id INTEGER NOT NULL REFERENCES ideologies (id),
    channel     TEXT    NOT NULL,
    support     INTEGER NOT NULL,
    rank        INTEGER NOT NULL,
    advanced    INTEGER NOT NULL CHECK (advanced IN (0,1)),
    PRIMARY KEY (election_id, stage, ideology_id)
) WITHOUT ROWID;

-- Slots 1..3 per (generation, channel) make ">3 per channel / >9 overall"
-- structurally impossible. The ideology must belong to the same channel.
CREATE TABLE final_nine (
    generation  INTEGER NOT NULL REFERENCES generations (number),
    channel     TEXT    NOT NULL CHECK (channel IN ('E','Z','D')),
    slot        INTEGER NOT NULL CHECK (slot BETWEEN 1 AND 3),
    ideology_id INTEGER NOT NULL,
    support     INTEGER NOT NULL,
    PRIMARY KEY (generation, channel, slot),
    UNIQUE (generation, ideology_id),
    FOREIGN KEY (ideology_id, channel) REFERENCES ideologies (id, channel)
) WITHOUT ROWID;
CREATE INDEX final_nine_ideology ON final_nine (ideology_id);

-- Historical results are append-only.
CREATE TRIGGER final_nine_no_update BEFORE UPDATE ON final_nine
BEGIN SELECT RAISE(ABORT, 'final_nine history is append-only'); END;
CREATE TRIGGER final_nine_no_delete BEFORE DELETE ON final_nine
BEGIN SELECT RAISE(ABORT, 'final_nine history is append-only'); END;

-- ---- Moderation -------------------------------------------------------------

CREATE TABLE reports (
    id          INTEGER PRIMARY KEY,
    reporter_id INTEGER NOT NULL REFERENCES users (id),
    target_kind TEXT    NOT NULL CHECK (target_kind IN ('note','ideology','user')),
    target_id   INTEGER NOT NULL,
    reason      TEXT    NOT NULL CHECK (length(reason) BETWEEN 3 AND 500),
    status      TEXT    NOT NULL DEFAULT 'open' CHECK (status IN ('open','actioned','dismissed')),
    created_at  INTEGER NOT NULL,
    resolved_by INTEGER REFERENCES users (id),
    resolved_at INTEGER,
    UNIQUE (reporter_id, target_kind, target_id)
);
CREATE INDEX reports_open ON reports (status, created_at);

CREATE TABLE appeals (
    id          INTEGER PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users (id),
    target_kind TEXT    NOT NULL CHECK (target_kind IN ('note','ideology','user')),
    target_id   INTEGER NOT NULL,
    message     TEXT    NOT NULL CHECK (length(message) BETWEEN 3 AND 1000),
    status      TEXT    NOT NULL DEFAULT 'open' CHECK (status IN ('open','granted','denied')),
    created_at  INTEGER NOT NULL,
    resolved_by INTEGER REFERENCES users (id),
    resolved_at INTEGER,
    admin_note  TEXT
);
CREATE UNIQUE INDEX appeals_one_open ON appeals (user_id, target_kind, target_id) WHERE status = 'open';

CREATE TABLE audit_log (
    id          INTEGER PRIMARY KEY,
    actor_id    INTEGER REFERENCES users (id),   -- NULL = system (scheduler)
    action      TEXT    NOT NULL,
    target_kind TEXT    NOT NULL,
    target_id   INTEGER NOT NULL,
    detail      TEXT    NOT NULL DEFAULT '',
    at          INTEGER NOT NULL
);
CREATE INDEX audit_at ON audit_log (at);
