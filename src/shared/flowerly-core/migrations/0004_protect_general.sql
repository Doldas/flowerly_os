-- General is a single protected system Flow per state database.
-- Reject ambiguous legacy General duplicates rather than choosing a winner.
CREATE UNIQUE INDEX idx_flows_unique_general
    ON flows(name) WHERE name = 'General';

-- An existing General retains its original ID; future name/id changes and
-- archiving are rejected at the SQLite boundary, not only in flowd.
CREATE TRIGGER protect_general_update
BEFORE UPDATE ON flows
WHEN OLD.name = 'General' AND (
    NEW.name != 'General' OR NEW.id != OLD.id OR NEW.state = 'ARCHIVED'
)
BEGIN
    SELECT RAISE(ABORT, 'protected General Flow');
END;

CREATE TRIGGER protect_general_delete
BEFORE DELETE ON flows
WHEN OLD.name = 'General'
BEGIN
    SELECT RAISE(ABORT, 'protected General Flow');
END;
