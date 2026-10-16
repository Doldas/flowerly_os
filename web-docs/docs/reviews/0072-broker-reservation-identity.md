# #72 broker reservation identity hardening

**Status:** Implemented, exact-commit CI and Arch kernel verification pending.

The merged broker kept in-memory ownership by Session name only. On `reap`, it opened the current directory at that name and sent `cgroup.kill`. A replaced name could therefore redirect the operation at a different cgroup. This change records the original directory's `st_dev/st_ino` at reserve and refuses killing a cgroup whose identity no longer matches. Regression tests exercise a replaced directory and absent reservation identity.

**Limits:** This does not solve all rename races between final verification and `rmdir`, reboot persistence, or malicious cgroup namespace manipulation. Directory inode reuse is another reason not to rely on this alone as an authorization proof. The name and in-memory ownership ledger are not crash durable. Production placement is still blocked pending separate trusted non-root broker UID, secure process lifecycle and cross-UID PID identity proof. The CI log for commit `edab2c95fadc77e6c9a737ed780758e3570e0b48` shows Rust formatting, Clippy and workspace tests passing, while the **positive Arch delegated cgroup acceptance fails** with `parent cgroup.procs is writable by session UID`. Do not weaken or bypass that test.

**Required release evidence:** unchanged Arch migration tests deny parent and management writes; setsid descendants and guardian crash cleanup prove containment; exact-SHA CI must pass. #55 untouched.
