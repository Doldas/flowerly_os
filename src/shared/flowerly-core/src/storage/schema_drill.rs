//! The G5 schema drill (ADR-0023 R4): one migration that only a test-image
//! `flowerlyd` built with the `g5-schema-drill` feature applies. It raises
//! `min_reader` past every released binary, so the real KVM acceptance can
//! activate irreversibly, refuse the way back, and restore the backup. No
//! release build contains it; `scripts/test-schema-compat.py` checks that this
//! module stays behind the feature.

use super::Migration;

pub(super) const MIGRATIONS: &[Migration] = &[Migration {
    version: 17,
    min_reader: 17,
    name: "g5_schema_drill",
    sql: include_str!("../../testdata/0017_g5_schema_drill.sql"),
}];
