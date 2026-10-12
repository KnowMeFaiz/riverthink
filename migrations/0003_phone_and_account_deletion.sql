-- Phone-number counter against multi-accounting (docs/DECISIONS.md D-16).
-- Only a keyed hash of the normalised number is stored, never the number itself.
-- NULL = legacy account created before phone numbers were required.
ALTER TABLE users ADD COLUMN phone_hash TEXT;
ALTER TABLE users ADD COLUMN deleted_at INTEGER;
CREATE INDEX users_phone ON users (phone_hash) WHERE phone_hash IS NOT NULL;

-- One phone number may back at most 2 accounts. Deleting an account clears its hash,
-- which frees the slot. Writers are serialised (BEGIN IMMEDIATE), so the count is race-free.
CREATE TRIGGER users_phone_limit_ins BEFORE INSERT ON users
WHEN NEW.phone_hash IS NOT NULL
 AND (SELECT COUNT(*) FROM users WHERE phone_hash = NEW.phone_hash) >= 2
BEGIN SELECT RAISE(ABORT, 'phone limit'); END;

CREATE TRIGGER users_phone_limit_upd BEFORE UPDATE OF phone_hash ON users
WHEN NEW.phone_hash IS NOT NULL
 AND NEW.phone_hash IS NOT OLD.phone_hash
 AND (SELECT COUNT(*) FROM users WHERE phone_hash = NEW.phone_hash AND id != NEW.id) >= 2
BEGIN SELECT RAISE(ABORT, 'phone limit'); END;
