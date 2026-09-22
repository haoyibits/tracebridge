//! Signal handling through signal-hook's safe API (no `unsafe` here).
//!
//! By default Ctrl-C ends the process with exit code 130 (the Python tool's
//! `SystemExit(130)`), and a closed stdout (`tracebridge config | head`) ends
//! it quietly with 141 instead of a panic on EPIPE. The long-running commands
//! release both: `rtt` handles Ctrl-C itself to restore the terminal, and the
//! DAP proxy shuts down through tokio's signal handling.

#[cfg(unix)]
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[cfg(unix)]
use signal_hook::{SigId, consts, flag, low_level};

/// The default actions installed by [`Handlers::install`].
pub struct Handlers {
    #[cfg(unix)]
    ids: Vec<SigId>,
}

impl Handlers {
    pub fn install() -> Handlers {
        #[cfg(unix)]
        {
            let always = Arc::new(AtomicBool::new(true));
            let ids = [
                (consts::SIGINT, 130),
                (consts::SIGPIPE, 128 + consts::SIGPIPE),
            ]
            .into_iter()
            .filter_map(|(signal, status)| {
                flag::register_conditional_shutdown(signal, status, always.clone()).ok()
            })
            .collect();
            Handlers { ids }
        }
        #[cfg(not(unix))]
        Handlers {}
    }

    /// Remove the default actions: Ctrl-C no longer exits, and writes to a
    /// closed pipe fail with EPIPE instead of ending the process.
    pub fn release(self) {
        #[cfg(unix)]
        for id in self.ids {
            low_level::unregister(id);
        }
    }
}

/// Set `flag` whenever Ctrl-C is pressed (used by `rtt`).
pub fn flag_on_interrupt(flag: &std::sync::Arc<AtomicBool>) {
    #[cfg(unix)]
    let _ = flag::register(consts::SIGINT, flag.clone());
    #[cfg(not(unix))]
    let _ = flag;
}
