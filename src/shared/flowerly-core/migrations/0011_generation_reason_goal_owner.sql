-- A Generation may name only a reason Goal with its own owner (#225 PR 4).
-- generation.create checks this in the inserting transaction and answers
-- `not_found`; this trigger is the database boundary for any other writer.
-- Existing rows are not rewritten.
CREATE TRIGGER generations_reason_goal_same_owner
BEFORE INSERT ON generations
WHEN NEW.reason_goal IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM goals
    WHERE goals.id = NEW.reason_goal AND goals.owner_class = NEW.owner_class
      AND goals.owner_uid IS NEW.owner_uid
)
BEGIN
    SELECT RAISE(ABORT, 'Generation reason Goal must have the same owner');
END;
