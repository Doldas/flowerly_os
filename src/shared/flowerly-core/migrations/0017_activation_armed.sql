-- Asynchronous arming (ADR-0023 G5). flowerlyd writes the PENDING row within
-- one IPC request and asks the slot broker to arm from a worker, because the
-- broker hashes both root slots first (seconds per GiB). armed_at records the
-- broker's ARMED answer, so a client can tell an armed request from one still
-- arming, or one whose answer was lost (PENDING, armed_at NULL, no worker).
-- Expand-only: binaries that know migration 16 still read this database.
ALTER TABLE generation_activations ADD COLUMN armed_at INTEGER
    CHECK (armed_at IS NULL OR armed_at >= requested_at);

-- The same guards as migration 15, and one more change a PENDING row may
-- take: armed_at from NULL to a time, with nothing else. It is set once and
-- never by a resolution.
DROP TRIGGER generation_activations_resolve_only;

CREATE TRIGGER generation_activations_resolve_only BEFORE UPDATE ON generation_activations
BEGIN
    SELECT RAISE(ABORT, 'a resolved activation is immutable') WHERE OLD.state <> 'PENDING';
    SELECT RAISE(ABORT, 'armed_at is armed once and never cleared')
        WHERE OLD.armed_at IS NOT NULL AND NEW.armed_at IS NOT OLD.armed_at;
    SELECT RAISE(ABORT, 'an activation is armed only while PENDING')
        WHERE NEW.state <> 'PENDING' AND NEW.armed_at IS NOT OLD.armed_at;
    SELECT RAISE(ABORT, 'an activation resolves to CONFIRMED, FELL_BACK or ABANDONED')
        WHERE NEW.state = 'PENDING'
          AND NOT (OLD.armed_at IS NULL AND NEW.armed_at IS NOT NULL
                   AND NEW.resolved_at IS NULL AND NEW.witness_boot_id IS NULL
                   AND NEW.witness_sequence IS NULL AND NEW.detail IS OLD.detail);
    SELECT RAISE(ABORT, 'only the resolution columns of an activation can change')
        WHERE NEW.id IS NOT OLD.id OR NEW.generation IS NOT OLD.generation
           OR NEW.previous_generation IS NOT OLD.previous_generation
           OR NEW.kind IS NOT OLD.kind OR NEW.actor_uid IS NOT OLD.actor_uid
           OR NEW.reason IS NOT OLD.reason OR NEW.operation_id IS NOT OLD.operation_id
           OR NEW.request_digest IS NOT OLD.request_digest
           OR NEW.expected_slot IS NOT OLD.expected_slot
           OR NEW.expected_root_partuuid IS NOT OLD.expected_root_partuuid
           OR NEW.ledger_sequence IS NOT OLD.ledger_sequence
           OR NEW.ledger_head IS NOT OLD.ledger_head
           OR NEW.irreversible IS NOT OLD.irreversible
           OR NEW.backup_sha256 IS NOT OLD.backup_sha256
           OR NEW.acknowledges IS NOT OLD.acknowledges
           OR NEW.requested_at IS NOT OLD.requested_at;
END;
