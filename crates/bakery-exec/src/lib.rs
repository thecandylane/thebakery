//! The room: a long-lived, provisioned environment for one workspace's
//! pinned tool spec. Human PTY and agent exec attach to the *same* room
//! through the same Seatbelt-prefixed `env -i` door — the room is not a
//! side effect of running a command.
//!
//! Rust only, macOS Seatbelt only — no Docker, no Apple `container`.

pub mod acp;
mod attach;
mod container;
mod credentials;
mod session;
mod spec;
mod toolchain;
mod whole_machine;

pub use acp::{
    claude_settings, seat_acp_home, spawn_boxed_acp, stage_acp_binary, BoxedAcpHandle, ACP_PATH_PREFIX_ENV,
    AGENT_STATE_DIRS, AGENT_WORK_DIRS, DOORS, DOOR_ENV, KEY_AGENTS_ENV, NET_ENV, PRIVATE_TMPDIR_ENV_VALUE, READABLE_ENV, ROOM_HOME_ENV_VALUE, SERVICES, SERVICES_ENV, SHUT_ENV, XCODE_ENV,
};
pub use attach::{
    agent_room_command, apple_developer_dirs, attach_exec, attach_exec_granted,
    attach_exec_workspace_rw, attach_pty, default_deny_paths, interactive_room_shell,
    kill_process_group, run_with_timeout, AttachOutput, ExtraGrants, JobRun, RoomShell, WorkspaceRun,
    DEFAULT_WORKSPACE_RUN_TIMEOUT, MAX_WORKSPACE_RUN_TIMEOUT,
};
pub use container::{
    container_ipv4, ensure_seat_network, first_ipv4, has_label, interactive_run_args, spawn_container_acp,
    start_container, stop_container, Backend, ContainerSeat, Share,
};
pub use credentials::{is_credential_filename, is_hidden_credential, is_package_certificate, CREDENTIAL_SURFACE_DENIED};
pub use session::{
    ensure_thin_room, open_cached_project_tools, provision, provision_project_tools, room_for_run,
    socket_runtime_dir, status, ProjectToolsStatus, SessionEnvironment, Status,
};
pub use spec::Spec;
pub use whole_machine::WholeMachine;

pub struct Probe {
    pub enforcement_tier: &'static str,
    pub seatbelt_available: bool,
}

/// What this backend can actually enforce, honestly. No host-process
/// fallback is offered when Seatbelt is missing — callers must fail closed.
pub fn probe() -> Probe {
    Probe {
        enforcement_tier: SessionEnvironment::ENFORCEMENT_TIER,
        seatbelt_available: std::path::Path::new("/usr/bin/sandbox-exec").is_file(),
    }
}
