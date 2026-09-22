//! Console output shared by the commands.

/// `info` in cli.py: a cyan `[tracebridge]` prefix on stdout.
pub fn info(message: &str) {
    println!("\x1b[1;36m[tracebridge]\x1b[0m {message}");
}

/// The absolute path of this executable, used wherever tracebridge calls itself.
pub fn current_exe() -> crate::errors::Result<std::path::PathBuf> {
    let exe = std::env::current_exe().map_err(|error| {
        crate::bridge_error!("cannot determine the tracebridge executable: {error}")
    })?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}
