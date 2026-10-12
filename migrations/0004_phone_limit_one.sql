-- One phone number = one account (docs/DECISIONS.md D-16, tightened from 2 to 1).
-- 0003 is already applied on existing databases, so its triggers are replaced here.
DROP TRIGGER users_phone_limit_ins;
DROP TRIGGER users_phone_limit_upd;

CREATE TRIGGER users_phone_limit_ins BEFORE INSERT ON users
WHEN NEW.phone_hash IS NOT NULL
 AND EXISTS (SELECT 1 FROM users WHERE phone_hash = NEW.phone_hash)
BEGIN SELECT RAISE(ABORT, 'phone limit'); END;

CREATE TRIGGER users_phone_limit_upd BEFORE UPDATE OF phone_hash ON users
WHEN NEW.phone_hash IS NOT NULL
 AND NEW.phone_hash IS NOT OLD.phone_hash
 AND EXISTS (SELECT 1 FROM users WHERE phone_hash = NEW.phone_hash AND id != NEW.id)
BEGIN SELECT RAISE(ABORT, 'phone limit'); END;