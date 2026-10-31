//! `--require-isolated-identity` (ADR-0022 §1, owner Q1): the remote
//! backend serves only as an unprivileged account holding no group besides
//! its primary one. systemd applies every supplementary group the account
//! is listed in, so adding `flowerly-provider` to another group would
//! otherwise widen what this process can read; with this check it fails
//! closed at startup instead.

/// Check the `Uid:`, `Gid:` and `Groups:` lines of `/proc/self/status`.
pub fn check_isolated(status: &str) -> Result<(), String> {
    let ids = |key: &str| -> Result<Vec<u32>, String> {
        let line = status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .ok_or_else(|| format!("process status has no `{key}` line"))?;
        line.split_whitespace()
            .map(|id| {
                id.parse::<u32>()
                    .map_err(|_| format!("unreadable id in `{key}`"))
            })
            .collect()
    };
    let uids = ids("Uid:")?;
    let gids = ids("Gid:")?;
    let groups = ids("Groups:")?;
    if uids.len() != 4 || gids.len() != 4 {
        return Err("process status has malformed Uid/Gid lines".to_owned());
    }
    if uids.contains(&0) || gids.contains(&0) {
        return Err("the remote backend refuses to run with uid or gid 0".to_owned());
    }
    let primary = gids[1];
    if let Some(extra) = groups.iter().find(|group| **group != primary) {
        return Err(format!(
            "the remote backend holds supplementary group {extra} besides its primary group {primary}; \
             remove the account from every other group"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_isolated;

    fn status(uid: &str, gid: &str, groups: &str) -> String {
        format!("Name:\tproviderd\nUid:\t{uid}\nGid:\t{gid}\nGroups:\t{groups}\nVmPeak:\t1 kB\n")
    }

    #[test]
    fn only_an_unprivileged_single_group_identity_is_accepted() {
        let isolated = "990\t990\t990\t990";
        let clients = "989\t989\t989\t989";
        assert!(check_isolated(&status(isolated, clients, "")).is_ok());
        assert!(check_isolated(&status(isolated, clients, "989")).is_ok());

        // Another group widens access: refused, naming the group (a gid,
        // not an account).
        let refused = check_isolated(&status(isolated, clients, "989 961")).unwrap_err();
        assert!(refused.contains("supplementary group 961"), "{refused}");
        assert!(check_isolated(&status(isolated, clients, "0")).is_err());
        assert!(check_isolated(&status("0\t0\t0\t0", clients, "")).is_err());
        assert!(check_isolated(&status("990\t0\t990\t990", clients, "")).is_err());
        assert!(check_isolated(&status(isolated, "989\t989\t0\t989", "")).is_err());

        // Unreadable status fails closed.
        assert!(check_isolated("Name:\tproviderd\n").is_err());
        assert!(check_isolated(&status(isolated, clients, "x")).is_err());
        assert!(check_isolated(&status("990", clients, "")).is_err());
    }
}
