//! The signal face (信号面, headless spec decision line; dsh
//! profile-boot.ts:307-308 twin).
//!
//! SIGINT maps to exit 130, SIGTERM maps to exit 0. The face owns
//! teardown throughout the startup window — signals registered before the
//! boot chain starts, so an arriving signal exits with the mapped code
//! from any phase.

/// The shutdown reason the signal face observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    /// User interrupt (exit 130).
    Interrupt,
    /// Supervisor stop request (exit 0).
    Terminate,
}

impl ShutdownReason {
    /// The dsh-mapped exit code.
    #[must_use]
    pub const fn exit_code(self) -> u32 {
        match self {
            Self::Interrupt => 130,
            Self::Terminate => 0,
        }
    }
}

/// Waits for the first SIGINT or SIGTERM.
///
/// tokio signal streams register the handlers at first poll, which the
/// boot chain precedes, so signals during the boot window are delivered
/// to the serve loop the moment it starts waiting.
///
///
/// # Panics
///
/// Panics when the runtime lacks the signal driver (never: the boot
/// runtime is built with `enable_all`).
pub async fn wait_shutdown() -> ShutdownReason {
    let (mut sigint, mut sigterm) = signal_pair();
    tokio::select! {
        _ = sigint.recv() => ShutdownReason::Interrupt,
        _ = sigterm.recv() => ShutdownReason::Terminate,
    }
}

/// Builds the SIGINT/SIGTERM stream pair (the signal driver panics
/// without `enable_all`; the boot runtime always enables it).
fn signal_pair() -> (tokio::signal::unix::Signal, tokio::signal::unix::Signal) {
    use tokio::signal::unix::{SignalKind, signal};
    (
        signal(SignalKind::interrupt()).expect("signal driver enabled"),
        signal(SignalKind::terminate()).expect("signal driver enabled"),
    )
}
