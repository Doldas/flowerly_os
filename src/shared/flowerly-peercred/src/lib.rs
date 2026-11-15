//! Kernel-attested supplementary groups of a connected Unix socket peer.
//!
//! `SO_PEERCRED` reports only the peer's uid, primary gid and pid.
//! `SO_PEERGROUPS` (Linux 4.13+) reports its supplementary groups from the
//! same credential snapshot the kernel captured at `connect()`, so the answer
//! cannot be affected by PID reuse, `/proc` visibility or later group changes
//! in the peer process. This crate is the only place in the workspace that
//! issues that `getsockopt`; `flowerly-core` stays `#![forbid(unsafe_code)]`.
//! [`peer_credentials`] reads `SO_PEERCRED` for callers without tokio's
//! safe wrapper (the slot broker, which serves one std socket).

#![deny(unsafe_op_in_unsafe_fn)]

use std::io;
use std::os::fd::AsFd;

/// Upper bound on the supplementary group list (the kernel's NGROUPS_MAX).
const MAX_GROUPS: usize = 65_536;

/// Supplementary group IDs of the process that connected `socket`, as
/// captured by the kernel at `connect()` time. The primary gid is not
/// included; read it from `SO_PEERCRED`.
///
/// Errors are returned unchanged. Callers performing authorization must
/// treat any error as "identity unknown" and refuse the peer.
#[cfg(target_os = "linux")]
pub fn peer_groups(socket: &impl AsFd) -> io::Result<Vec<u32>> {
    peer_groups_from(socket, 32)
}

#[cfg(target_os = "linux")]
fn peer_groups_from(socket: &impl AsFd, initial: usize) -> io::Result<Vec<u32>> {
    use std::os::fd::AsRawFd;

    const GID_BYTES: usize = std::mem::size_of::<libc::gid_t>();
    let fd = socket.as_fd().as_raw_fd();
    let mut groups: Vec<libc::gid_t> = vec![0; initial];
    loop {
        let capacity = groups.len() * GID_BYTES;
        let mut len = libc::socklen_t::try_from(capacity)
            .map_err(|_| io::Error::other("supplementary group buffer too large"))?;
        // SAFETY: `fd` is a live descriptor borrowed from `socket` for the
        // duration of this call. The kernel writes at most `len` bytes into
        // the buffer, which points to `groups.len()` initialized `gid_t`
        // values (exactly `capacity` bytes), and updates `len` in place.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERGROUPS,
                groups.as_mut_ptr().cast::<libc::c_void>(),
                &mut len,
            )
        };
        let len = len as usize;
        if rc == 0 {
            if len > capacity || !len.is_multiple_of(GID_BYTES) {
                return Err(io::Error::other("kernel returned a malformed group list"));
            }
            groups.truncate(len / GID_BYTES);
            return Ok(groups);
        }
        let error = io::Error::last_os_error();
        // On ERANGE the kernel reports the required size in `len`.
        let needed = len / GID_BYTES;
        if error.raw_os_error() == Some(libc::ERANGE)
            && needed > groups.len()
            && needed <= MAX_GROUPS
        {
            groups.resize(needed, 0);
            continue;
        }
        return Err(error);
    }
}

/// Process id, uid and primary gid of a connected Unix socket peer, from
/// `SO_PEERCRED`, captured by the kernel at `connect()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

/// `SO_PEERCRED` of `socket`. Any error means "identity unknown": refuse.
#[cfg(target_os = "linux")]
pub fn peer_credentials(socket: &impl AsFd) -> io::Result<PeerCredentials> {
    use std::os::fd::AsRawFd;

    let fd = socket.as_fd().as_raw_fd();
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let size = std::mem::size_of::<libc::ucred>();
    let mut len = libc::socklen_t::try_from(size).map_err(|_| io::Error::other("ucred size"))?;
    // SAFETY: `fd` is a live descriptor borrowed from `socket` for the
    // duration of this call; the kernel writes at most `len` bytes into
    // `credentials`, a properly aligned, initialized `ucred` of that size.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast::<libc::c_void>(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if len as usize != size {
        return Err(io::Error::other("kernel returned a malformed ucred"));
    }
    Ok(PeerCredentials {
        pid: u32::try_from(credentials.pid).map_err(|_| io::Error::other("invalid peer pid"))?,
        uid: credentials.uid,
        gid: credentials.gid,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn peer_credentials(_socket: &impl AsFd) -> io::Result<PeerCredentials> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "SO_PEERCRED requires Linux",
    ))
}

/// Supplementary groups are unavailable off Linux; authorization fails closed.
#[cfg(not(target_os = "linux"))]
pub fn peer_groups(_socket: &impl AsFd) -> io::Result<Vec<u32>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "SO_PEERGROUPS requires Linux",
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    /// The connecting process's own supplementary groups, from /proc.
    fn own_groups() -> Vec<u32> {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let line = status
            .lines()
            .find_map(|line| line.strip_prefix("Groups:"))
            .unwrap();
        let mut groups: Vec<u32> = line
            .split_whitespace()
            .map(|g| g.parse().unwrap())
            .collect();
        groups.sort_unstable();
        groups
    }

    #[test]
    fn reports_the_connecting_process_groups() {
        let (left, right) = UnixStream::pair().unwrap();
        for socket in [&left, &right] {
            let mut groups = peer_groups(socket).unwrap();
            groups.sort_unstable();
            assert_eq!(groups, own_groups());
        }
    }

    #[test]
    fn grows_the_buffer_when_the_kernel_reports_erange() {
        let (left, _right) = UnixStream::pair().unwrap();
        let mut groups = peer_groups_from(&left, 0).unwrap();
        groups.sort_unstable();
        assert_eq!(groups, own_groups());
    }

    #[test]
    fn reports_the_connecting_process_credentials() {
        let (left, right) = UnixStream::pair().unwrap();
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let field = |name: &str| -> u32 {
            status
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap()
        };
        for socket in [&left, &right] {
            let peer = peer_credentials(socket).unwrap();
            assert_eq!(peer.uid, field("Uid:"));
            assert_eq!(peer.gid, field("Gid:"));
            assert_eq!(peer.pid, std::process::id());
        }
        let file = std::fs::File::open("/proc/self/status").unwrap();
        assert_eq!(
            peer_credentials(&file).unwrap_err().raw_os_error(),
            Some(libc::ENOTSOCK)
        );
    }

    #[test]
    fn non_socket_descriptor_is_an_error() {
        let file = std::fs::File::open("/proc/self/status").unwrap();
        let error = peer_groups(&file).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTSOCK));
    }
}
