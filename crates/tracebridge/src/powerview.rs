//! Starting or reusing PowerView and installing its toolbar (powerview.py).

use std::fs::OpenOptions;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::errors::{BridgeError, Result};
use crate::remote::{CONNECT_TIMEOUT, connect_debugger};
use crate::{bail, bridge_error, t32config, ui};

const TOOLBAR_CMM: &str = include_str!("../assets/toolbar.cmm");
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const TOOLBAR_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// `port_open`: can a TCP connection to 127.0.0.1:`port` be made within 0.2 s?
pub fn port_open(port: u16) -> bool {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
}

/// `require_file`.
pub fn require_file(path: &Path, description: &str, executable: bool) -> Result<()> {
    if !path.is_file() {
        bail!("{description} not found: {}", path.display());
    }
    if executable && !is_executable(path) {
        bail!("{description} not executable: {}", path.display());
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    nix::unistd::access(path, nix::unistd::AccessFlags::X_OK).is_ok()
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// `verify_rcl`: the port must answer as a TRACE32 Remote API.
pub fn verify_rcl(config: &Config) -> Result<()> {
    connect_debugger(config, Duration::from_millis(500)).map(drop)
}

/// A started process whose exit can be polled (`subprocess.Popen.poll`).
pub trait Process {
    /// `None` while running; the exit code, or minus the signal number.
    fn poll(&mut self) -> Option<i32>;
}

impl Process for Child {
    fn poll(&mut self) -> Option<i32> {
        match self.try_wait() {
            Ok(Some(status)) => Some(exit_code(status)),
            _ => None,
        }
    }
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return -signal;
        }
    }
    status.code().unwrap_or(-1)
}

/// Everything `start_powerview` touches outside this process; replaced in tests.
pub trait Host {
    fn port_open(&mut self, port: u16) -> bool;
    fn verify_rcl(&mut self, config: &Config) -> Result<()>;
    fn spawn(&mut self, config: &Config, log_path: &Path) -> Result<Box<dyn Process>>;
    fn install_toolbar(&mut self, config: &Config) -> Result<()>;
    fn sleep(&mut self, duration: Duration);
    fn now(&mut self) -> Instant;
}

pub struct RealHost;

impl Host for RealHost {
    fn port_open(&mut self, port: u16) -> bool {
        port_open(port)
    }
    fn verify_rcl(&mut self, config: &Config) -> Result<()> {
        verify_rcl(config)
    }
    fn spawn(&mut self, config: &Config, log_path: &Path) -> Result<Box<dyn Process>> {
        Ok(Box::new(spawn_powerview(config, log_path)?))
    }
    fn install_toolbar(&mut self, config: &Config) -> Result<()> {
        install_toolbar(config)
    }
    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration)
    }
    fn now(&mut self) -> Instant {
        Instant::now()
    }
}

/// The PowerView command line. When config.t32 does not enable the Remote
/// API, it is enabled on the command line with the configured port.
pub fn powerview_arguments(config: &Config) -> Vec<String> {
    let mut arguments = vec![
        "-c".to_string(),
        config.t32_config.to_string_lossy().into_owned(),
    ];
    if t32config::rcl_settings(&config.t32_config).is_none() {
        arguments.push(format!("--t32-api-rcl=TCP:{}", config.rcl_port));
    }
    arguments
}

/// Start PowerView detached from this process: new session, stdin from
/// /dev/null, stdout and stderr appended to the log, working directory at the
/// project root.
fn spawn_powerview(config: &Config, log_path: &Path) -> Result<Child> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| bridge_error!("cannot open {}: {error}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|error| bridge_error!("cannot open {}: {error}", log_path.display()))?;
    let mut command = Command::new(&config.t32_binary);
    command
        .args(powerview_arguments(config))
        .current_dir(&config.project_dir)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and touches no Rust state.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid()
                    .map(drop)
                    .map_err(std::io::Error::from)
            });
        }
    }
    command.spawn().map_err(|error| {
        bridge_error!(
            "cannot start PowerView {}: {error}",
            config.t32_binary.display()
        )
    })
}

/// `wait_for_powerview`.
pub fn wait_for_powerview(
    config: &Config,
    process: &mut dyn Process,
    log_path: &Path,
    host: &mut dyn Host,
    timeout: Duration,
) -> Result<()> {
    let deadline = host.now() + timeout;
    let mut last_rcl_error: Option<BridgeError> = None;
    let mut launcher_exited_cleanly = false;
    while host.now() <= deadline {
        match process.poll() {
            Some(0) => {
                // On macOS the t32*-qt launcher delegates to `open`, which exits
                // successfully while the application continues starting.
                launcher_exited_cleanly = true;
            }
            Some(code) => bail!(
                "PowerView exited with code {code} before RCL became ready; check {}",
                log_path.display()
            ),
            None => {}
        }
        if host.port_open(config.rcl_port) {
            match host.verify_rcl(config) {
                Ok(()) => return Ok(()),
                Err(error) => last_rcl_error = Some(error),
            }
        }
        host.sleep(POLL_INTERVAL);
    }
    let detail = last_rcl_error
        .map(|error| format!(": {error}"))
        .unwrap_or_default();
    let launcher = if launcher_exited_cleanly {
        " (the launcher exited successfully, but RCL never became ready)"
    } else {
        ""
    };
    bail!(
        "PowerView did not become ready on RCL port {}{launcher}{detail}; check RCL=NETTCP in {} and {}",
        config.rcl_port,
        config.t32_config.display(),
        log_path.display()
    )
}

/// `start_powerview`: reuse a running PowerView or start one. Returns true
/// when a new process was started.
pub fn start_powerview(config: &Config) -> Result<bool> {
    start_powerview_with(config, &mut RealHost)
}

pub fn start_powerview_with(config: &Config, host: &mut dyn Host) -> Result<bool> {
    if host.port_open(config.rcl_port) {
        host.verify_rcl(config).map_err(|error| {
            bridge_error!(
                "port {} is open but is not a usable TRACE32 RCL endpoint: {error}",
                config.rcl_port
            )
        })?;
        return Ok(false);
    }

    require_file(&config.t32_binary, "PowerView", true)?;
    require_file(&config.t32_config, "TRACE32 config", false)?;
    config.ensure_run_dir()?;
    let log_path = config.run_dir.join("powerview.log");
    let mut process = host.spawn(config, &log_path)?;
    wait_for_powerview(config, process.as_mut(), &log_path, host, READY_TIMEOUT)?;
    host.install_toolbar(config)?;
    Ok(true)
}

/// The `DO` arguments of toolbar.cmm: the script, the executable and the config.
pub fn toolbar_command(script: &Path, exe: &Path, config_file: &Path) -> String {
    format!(
        "\"{}\" \"{}\" \"{}\"",
        script.display(),
        exe.display(),
        config_file.display()
    )
}

/// Write toolbar.cmm into the run directory and return its path.
fn write_toolbar(config: &Config) -> Result<PathBuf> {
    config.ensure_run_dir()?;
    let path = config.run_dir.join("toolbar.cmm");
    std::fs::write(&path, TOOLBAR_CMM)
        .map_err(|error| bridge_error!("cannot write {}: {error}", path.display()))?;
    Ok(path)
}

/// `install_toolbar`: add the Flash and Load ELF buttons through RCL.
pub fn install_toolbar(config: &Config) -> Result<()> {
    let exe = ui::current_exe()?;
    if exe.to_string_lossy().contains(['"', '\r', '\n']) {
        bail!("tracebridge executable path is unsafe for a TRACE32 command");
    }
    let script = write_toolbar(config)?;
    let command = toolbar_command(&script, &exe, &config.config_file);

    let deadline = Instant::now() + TOOLBAR_CONNECT_TIMEOUT;
    let mut last_error = None;
    let mut debugger = None;
    while Instant::now() <= deadline {
        match connect_debugger(config, CONNECT_TIMEOUT) {
            Ok(connected) => {
                debugger = Some(connected);
                break;
            }
            Err(error) => {
                last_error = Some(error);
                thread::sleep(POLL_INTERVAL);
            }
        }
    }
    let Some(mut debugger) = debugger else {
        bail!(
            "could not connect to install the toolbar: {}",
            last_error.map(|e| e.to_string()).unwrap_or_default()
        );
    };
    debugger
        .cmm(&command, Some(Duration::from_secs(10)))
        .map_err(|error| bridge_error!("could not install the PowerView toolbar: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::tests::make_config;
    use std::collections::VecDeque;

    struct FakeProcess(Option<i32>);

    impl Process for FakeProcess {
        fn poll(&mut self) -> Option<i32> {
            self.0
        }
    }

    struct FakeHost {
        ports: VecDeque<bool>,
        verify: Vec<Result<()>>,
        verified: usize,
        exit: Option<i32>,
        spawned: Vec<PathBuf>,
        toolbar: usize,
        clock: Instant,
    }

    impl FakeHost {
        fn new(ports: &[bool]) -> FakeHost {
            FakeHost {
                ports: ports.iter().copied().collect(),
                verify: Vec::new(),
                verified: 0,
                exit: None,
                spawned: Vec::new(),
                toolbar: 0,
                clock: Instant::now(),
            }
        }
    }

    impl Host for FakeHost {
        fn port_open(&mut self, _port: u16) -> bool {
            self.ports.pop_front().unwrap_or(false)
        }
        fn verify_rcl(&mut self, _config: &Config) -> Result<()> {
            self.verified += 1;
            if self.verify.is_empty() {
                Ok(())
            } else {
                self.verify.remove(0)
            }
        }
        fn spawn(&mut self, _config: &Config, log_path: &Path) -> Result<Box<dyn Process>> {
            self.spawned.push(log_path.to_path_buf());
            Ok(Box::new(FakeProcess(self.exit)))
        }
        fn install_toolbar(&mut self, _config: &Config) -> Result<()> {
            self.toolbar += 1;
            Ok(())
        }
        fn sleep(&mut self, duration: Duration) {
            self.clock += duration;
        }
        fn now(&mut self) -> Instant {
            self.clock
        }
    }

    fn executable_fixture() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let config = make_config(dir.path());
        std::fs::write(&config.t32_binary, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config.t32_binary, std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        std::fs::write(&config.t32_config, "RCL=NETTCP\nPORT=20000\n").unwrap();
        (dir, config)
    }

    // test_powerview.py: test_toolbar_is_installed_through_rcl
    #[test]
    fn toolbar_command_passes_script_exe_and_config() {
        assert_eq!(
            toolbar_command(
                Path::new("/p/.tracebridge/toolbar.cmm"),
                Path::new("/usr/local/bin/tracebridge"),
                Path::new("/p/trace32.toml"),
            ),
            "\"/p/.tracebridge/toolbar.cmm\" \"/usr/local/bin/tracebridge\" \"/p/trace32.toml\""
        );
    }

    #[test]
    fn toolbar_script_calls_tracebridge_with_its_config() {
        assert!(TOOLBAR_CMM.contains(
            "TOOLITEM \"Flash\"    \":FLASH\" \"OS.Command \"\"&exe\"\" --config \"\"&config\"\" flash &\""
        ));
        assert!(TOOLBAR_CMM.contains("--config \"\"&config\"\" load &\""));
        let dir = tempfile::tempdir().unwrap();
        let config = make_config(dir.path());
        let path = write_toolbar(&config).unwrap();
        assert_eq!(path, config.run_dir.join("toolbar.cmm"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), TOOLBAR_CMM);
    }

    // test_powerview.py: test_existing_port_must_answer_as_rcl
    #[test]
    fn existing_port_must_answer_as_rcl() {
        let (_dir, config) = executable_fixture();
        let mut host = FakeHost::new(&[true]);
        host.verify.push(Err(bridge_error!("not RCL")));
        let error = start_powerview_with(&config, &mut host).unwrap_err();
        assert_eq!(
            error.0,
            "port 20000 is open but is not a usable TRACE32 RCL endpoint: not RCL"
        );
        assert!(host.spawned.is_empty());
    }

    #[test]
    fn running_powerview_is_reused() {
        let (_dir, config) = executable_fixture();
        let mut host = FakeHost::new(&[true]);
        assert!(!start_powerview_with(&config, &mut host).unwrap());
        assert!(host.spawned.is_empty());
        assert_eq!(host.toolbar, 0);
    }

    // test_powerview.py: test_process_exit_is_reported_without_waiting_for_timeout
    #[test]
    fn process_exit_is_reported_without_waiting_for_timeout() {
        let (_dir, config) = executable_fixture();
        let mut host = FakeHost::new(&[false]);
        host.exit = Some(7);
        let error = start_powerview_with(&config, &mut host).unwrap_err();
        assert!(
            error.0.starts_with("PowerView exited with code 7"),
            "{error}"
        );
        assert_eq!(host.spawned, [config.run_dir.join("powerview.log")]);
        assert!(config.run_dir.join(".gitignore").is_file());
    }

    #[test]
    fn new_powerview_gets_the_toolbar() {
        let (_dir, config) = executable_fixture();
        let mut host = FakeHost::new(&[false, false, true]);
        assert!(start_powerview_with(&config, &mut host).unwrap());
        assert_eq!(host.toolbar, 1);
    }

    #[test]
    fn missing_binary_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let config = make_config(dir.path());
        let error = start_powerview_with(&config, &mut FakeHost::new(&[false])).unwrap_err();
        assert_eq!(
            error.0,
            format!("PowerView not found: {}", config.t32_binary.display())
        );
    }

    // test_powerview.py: test_successful_launcher_exit_waits_for_detached_application
    #[test]
    fn successful_launcher_exit_waits_for_detached_application() {
        let dir = tempfile::tempdir().unwrap();
        let config = make_config(dir.path());
        let mut host = FakeHost::new(&[false, true]);
        let mut process = FakeProcess(Some(0));
        wait_for_powerview(
            &config,
            &mut process,
            Path::new("/tmp/powerview.log"),
            &mut host,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(host.verified, 1);
    }

    #[test]
    fn timeout_mentions_launcher_and_last_rcl_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = make_config(dir.path());
        let mut host = FakeHost::new(&[true; 20]);
        host.verify = (0..20).map(|_| Err(bridge_error!("refused"))).collect();
        let mut process = FakeProcess(Some(0));
        let error = wait_for_powerview(
            &config,
            &mut process,
            Path::new("/tmp/powerview.log"),
            &mut host,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(
            error.0,
            format!(
                "PowerView did not become ready on RCL port 20000 (the launcher exited \
                 successfully, but RCL never became ready): refused; check RCL=NETTCP in {} \
                 and /tmp/powerview.log",
                config.t32_config.display()
            )
        );
    }

    #[test]
    fn remote_api_is_enabled_on_the_command_line_when_config_lacks_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = make_config(dir.path());
        std::fs::write(&config.t32_config, "PBI=\nUSB\n").unwrap();
        let arguments = powerview_arguments(&config);
        assert_eq!(arguments[2], "--t32-api-rcl=TCP:20000");
        std::fs::write(&config.t32_config, "RCL=NETTCP\nPORT=20000\n").unwrap();
        assert_eq!(powerview_arguments(&config).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn spawned_process_runs_in_project_dir_and_logs() {
        let (_dir, mut config) = executable_fixture();
        let script = config.config_dir.join("fake-powerview.sh");
        std::fs::write(&script, "#!/bin/sh\npwd\necho \"args: $*\"\necho err >&2\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        config.t32_binary = script;
        config.ensure_run_dir().unwrap();
        let log = config.run_dir.join("powerview.log");
        let mut child = spawn_powerview(&config, &log).unwrap();
        assert!(child.wait().unwrap().success());
        let text = std::fs::read_to_string(&log).unwrap();
        let project = std::fs::canonicalize(&config.project_dir).unwrap();
        assert!(
            text.contains(&project.to_string_lossy().into_owned()),
            "{text}"
        );
        assert!(text.contains("args: -c "), "{text}");
        assert!(text.contains("err"), "{text}");
    }
}
