# shell

The user-facing shell is the GUI shell of Flowerly OS (see
`docs/AGENTIC_OS_UPDATE_03_UX_SPECIFICATION.md` in docs/). Requirements:

- ambient, persistent, context-aware interactions
- flows, sessions, goals and generations presented as first-class objects
- humans and agents drive the same underlying daemons (via `agenticctl` today)

Nothing implemented yet. The control-plane surface it will sit on is the
`agenticctl` CLI + Unix-socket IPC.