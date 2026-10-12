-- Channel rooms: an Ambinote entry may be posted into the open discussion room of
-- #E, #Z or #D. NULL = general Ambinote (no room). Anyone may read and write in any
-- room regardless of their home channel; the home channel only restricts voting
-- for, and founding of, ideologies (docs/DECISIONS.md D-04, D-15).
-- Existing entries keep room = NULL.
ALTER TABLE notes ADD COLUMN room TEXT CHECK (room IS NULL OR room IN ('E','Z','D'));
CREATE INDEX notes_room ON notes (room, id);
