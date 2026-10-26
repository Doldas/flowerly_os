-- Owner-scoped compute data (#225 PR 5). Profiles are private to their user
-- owner or shared read-only as `system`; workloads and usage are private.
--
-- Profile names were globally unique, so one user's `profile.create` could
-- learn that another user's name exists from a conflict. Names are now unique
-- per owner. A global name is unique per owner too, so this cannot fail on
-- existing rows. COALESCE keeps one namespace for each ownerless class.
DROP INDEX IF EXISTS idx_ai_profiles_name;
CREATE UNIQUE INDEX idx_ai_profiles_owner_name
    ON ai_profiles(owner_class, COALESCE(owner_uid, -1), name);

-- A workload may use only a profile its owner can see: its own or a system
-- profile. The daemon checks this in the admission path and answers
-- `not_found`; these triggers are the database boundary for any other writer.
-- Existing rows are not rewritten.
CREATE TRIGGER inference_workloads_profile_visible
BEFORE INSERT ON inference_workloads
WHEN NOT EXISTS (
    SELECT 1 FROM ai_profiles AS p
    WHERE p.id = NEW.profile_id AND (
        p.owner_class = 'system'
        OR (p.owner_class = NEW.owner_class AND p.owner_uid IS NEW.owner_uid))
)
BEGIN
    SELECT RAISE(ABORT, 'workload profile must be the owner''s own or a system profile');
END;

-- Usage belongs to the owner of the workload it accounts for.
CREATE TRIGGER inference_usage_workload_same_owner
BEFORE INSERT ON inference_usage
WHEN NOT EXISTS (
    SELECT 1 FROM inference_workloads AS w
    WHERE w.id = NEW.workload_id AND w.owner_class = NEW.owner_class
      AND w.owner_uid IS NEW.owner_uid
)
BEGIN
    SELECT RAISE(ABORT, 'usage must have the same owner as its workload');
END;
