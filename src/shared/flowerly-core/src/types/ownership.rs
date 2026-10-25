//! Authoritative object ownership (#225).
//!
//! `created_by` on a record is immutable creator audit text. Access control
//! uses [`Owner`], stored in the `owner_class` / `owner_uid` columns, and the
//! kernel-attested [`Caller`] of each IPC request. A caller is passed
//! explicitly to every operation; there is no ambient "current user".

/// Who owns a stored object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Owner {
    /// Owned by one Linux uid.
    User(u32),
    /// A shared system object such as the General Flow.
    System,
    /// Owner unknown (legacy or malformed). Protected: visible to no
    /// ordinary user until an audited administrator claim. Also the default,
    /// so a write path that forgets to record an owner fails closed.
    #[default]
    Unresolved,
}

impl Owner {
    /// The `owner_class` column value.
    #[must_use]
    pub fn class(self) -> &'static str {
        match self {
            Self::User(_) => "user",
            Self::System => "system",
            Self::Unresolved => "unresolved",
        }
    }

    /// The `owner_uid` column value.
    #[must_use]
    pub fn uid(self) -> Option<u32> {
        match self {
            Self::User(uid) => Some(uid),
            Self::System | Self::Unresolved => None,
        }
    }

    /// Decode stored columns. Anything inconsistent is `Unresolved`, never
    /// a guessed owner.
    #[must_use]
    pub fn from_columns(class: &str, uid: Option<i64>) -> Self {
        match (class, uid) {
            ("user", Some(uid)) => u32::try_from(uid)
                .ok()
                .filter(|uid| *uid != u32::MAX)
                .map_or(Self::Unresolved, Self::User),
            ("system", None) => Self::System,
            _ => Self::Unresolved,
        }
    }
}

/// The kernel-attested uid of an IPC request. Construct it only from the
/// peer credentials of an accepted connection (or in tests), never from
/// request JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caller {
    uid: u32,
}

impl Caller {
    #[must_use]
    pub const fn new(uid: u32) -> Self {
        Self { uid }
    }

    #[must_use]
    pub fn uid(self) -> u32 {
        self.uid
    }

    /// The owner of an object this caller creates.
    #[must_use]
    pub fn owner(self) -> Owner {
        Owner::User(self.uid)
    }

    /// The `created_by` audit label for this caller.
    #[must_use]
    pub fn actor(self) -> String {
        format!("uid:{}", self.uid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_columns_round_trip_and_malformed_values_are_unresolved() {
        for owner in [
            Owner::User(0),
            Owner::User(1000),
            Owner::System,
            Owner::Unresolved,
        ] {
            let uid = owner.uid().map(i64::from);
            assert_eq!(Owner::from_columns(owner.class(), uid), owner);
        }
        for (class, uid) in [
            ("user", None),
            ("user", Some(-1)),
            ("user", Some(i64::from(u32::MAX))),
            ("user", Some(1 << 40)),
            ("system", Some(5)),
            ("unresolved", Some(5)),
            ("admin", None),
            ("", None),
        ] {
            assert_eq!(
                Owner::from_columns(class, uid),
                Owner::Unresolved,
                "{class} {uid:?}"
            );
        }
        assert_eq!(Owner::default(), Owner::Unresolved);
    }

    #[test]
    fn caller_owns_what_it_creates() {
        let caller = Caller::new(998);
        assert_eq!(caller.owner(), Owner::User(998));
        assert_eq!(caller.actor(), "uid:998");
    }
}
