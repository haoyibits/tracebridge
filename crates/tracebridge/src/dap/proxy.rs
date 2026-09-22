//! DAP compatibility proxy in front of t32debugadapter (dap/proxy.py).
//!
//! The IDE connects to `dap_port`; every message is forwarded to
//! t32debugadapter on `dap_backend_port` and back, except:
//!
//! * `restart` is handled here: the target is reset through RCL (`Break`,
//!   `SYStem.Mode Up`), then the proxy asks the adapter to `continue` and
//!   answers the IDE itself.
//! * `variables` requests for a Locals scope are answered with an empty list.
//!   Some t32debugadapter versions fail to read locals on certain FreeRTOS
//!   interrupt stack frames ("Invalid letter code") and then exit, which ends
//!   the whole debug session (see the Python tool's commit bd20db4 and its
//!   README). Watch expressions, registers, the call stack, breakpoints and
//!   stepping are forwarded normally.
//! * `launch` with `"request": "attach"` in its arguments is forwarded as
//!   `attach` and its response renamed back to `launch`. JetBrains IDEs
//!   (LSP4IJ) can only start the adapter themselves in launch mode, while
//!   t32debugadapter only implements attach. This rule is new in tracebridge.
//!
//! Requests the proxy sends on its own are answered inside the proxy and never
//! reach the IDE. Writes to each side are serialised.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, oneshot, watch};
use tokio::task::JoinHandle;

use super::protocol::{DapDecoder, DapProtocolError, Message, encode_message};
use crate::config::Config;
use crate::errors::Result;
use crate::powerview::{port_open, require_file};
use crate::remote::reset_and_stop;
use crate::{bail, bridge_error};

const READ_SIZE: usize = 65536;
const INTERNAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const FIRST_SEQUENCE: i64 = 1_000_000;

/// Resets the target (`Break`, `SYStem.Mode Up`); replaced in tests.
pub type ResetFn = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = std::result::Result<(), String>> + Send>> + Send + Sync,
>;

fn log(message: impl std::fmt::Display) {
    println!("[tracebridge] {message}");
}

/// Python truthiness of a JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

fn truthy_str(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::String(text)) if !text.is_empty() => Some(text),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("{0}")]
    Protocol(#[from] DapProtocolError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// The part of `DapProxy` a session needs: the sequence counter and the reset.
pub struct ProxyCore {
    sequence: AtomicI64,
    reset: ResetFn,
}

impl ProxyCore {
    pub fn new(reset: ResetFn) -> Arc<ProxyCore> {
        Arc::new(ProxyCore {
            sequence: AtomicI64::new(FIRST_SEQUENCE),
            reset,
        })
    }

    fn next_sequence(&self) -> i64 {
        self.sequence.fetch_add(1, Ordering::SeqCst)
    }

    /// `DapProxy.response`; `message` and `body` are always present, as null.
    fn response(
        &self,
        request: &Message,
        success: bool,
        message: Option<String>,
        body: Option<Value>,
    ) -> Message {
        let mut response = Message::new();
        response.insert("seq".into(), json!(self.next_sequence()));
        response.insert("type".into(), json!("response"));
        response.insert(
            "request_seq".into(),
            request.get("seq").cloned().unwrap_or(Value::Null),
        );
        response.insert("success".into(), json!(success));
        response.insert(
            "command".into(),
            request.get("command").cloned().unwrap_or(Value::Null),
        );
        response.insert("message".into(), message.map_or(Value::Null, Value::String));
        response.insert("body".into(), body.unwrap_or(Value::Null));
        response
    }
}

async fn send(writer: &Mutex<OwnedWriteHalf>, message: &Message) -> std::io::Result<()> {
    let frame = encode_message(message)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let mut writer = writer.lock().await;
    writer.write_all(&frame).await?;
    writer.flush().await
}

#[derive(Default)]
struct SessionState {
    client_commands: HashMap<i64, String>,
    /// Requests sent by the proxy; the sender is taken by the first answer.
    internal_requests: HashMap<i64, Option<oneshot::Sender<std::result::Result<Message, String>>>>,
    local_references: HashSet<i64>,
    launch_rewrites: HashSet<i64>,
}

/// One IDE connection (`DapSession`).
pub struct Session {
    proxy: Arc<ProxyCore>,
    client_writer: Mutex<OwnedWriteHalf>,
    backend_writer: Mutex<OwnedWriteHalf>,
    state: StdMutex<SessionState>,
    background: StdMutex<Vec<JoinHandle<()>>>,
}

impl Session {
    fn start_background(self: &Arc<Self>, future: impl Future<Output = ()> + Send + 'static) {
        let mut background = self.background.lock().unwrap();
        background.retain(|task| !task.is_finished());
        background.push(tokio::spawn(future));
    }

    async fn to_client(&self, message: &Message) -> std::io::Result<()> {
        send(&self.client_writer, message).await
    }

    async fn to_backend(&self, message: &Message) -> std::io::Result<()> {
        send(&self.backend_writer, message).await
    }

    /// `backend_request`: send a request of the proxy's own and wait (5 s) for
    /// its answer, which is not forwarded to the IDE.
    async fn backend_request(
        &self,
        command: &str,
        arguments: Value,
    ) -> std::result::Result<Message, String> {
        let sequence = self.proxy.next_sequence();
        let (sender, receiver) = oneshot::channel();
        self.state
            .lock()
            .unwrap()
            .internal_requests
            .insert(sequence, Some(sender));
        let mut request = Message::new();
        request.insert("seq".into(), json!(sequence));
        request.insert("type".into(), json!("request"));
        request.insert("command".into(), json!(command));
        request.insert("arguments".into(), arguments);
        let result = match self.to_backend(&request).await {
            Err(error) => Err(error.to_string()),
            Ok(()) => match tokio::time::timeout(INTERNAL_REQUEST_TIMEOUT, receiver).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) => Err("the debug session ended".to_string()),
                Err(_) => Err(format!(
                    "t32debugadapter did not answer {command} within {}s",
                    INTERNAL_REQUEST_TIMEOUT.as_secs()
                )),
            },
        };
        self.state
            .lock()
            .unwrap()
            .internal_requests
            .remove(&sequence);
        result
    }

    /// `restart`: reset through RCL, then let the adapter continue.
    async fn restart(self: Arc<Self>, request: Message) {
        log("restart: reset the target, then continue");
        let outcome = async {
            (self.proxy.reset)().await?;
            self.state.lock().unwrap().local_references.clear();
            self.backend_request("continue", json!({"threadId": 0}))
                .await?;
            Ok::<(), String>(())
        }
        .await;
        let sent = match outcome {
            Ok(()) => {
                let response = self.proxy.response(&request, true, None, None);
                let mut event = Message::new();
                event.insert("seq".into(), json!(self.proxy.next_sequence()));
                event.insert("type".into(), json!("event"));
                event.insert("event".into(), json!("continued"));
                event.insert(
                    "body".into(),
                    json!({"threadId": 0, "allThreadsContinued": true}),
                );
                match self.to_client(&response).await {
                    Ok(()) => self.to_client(&event).await,
                    Err(error) => Err(error),
                }
            }
            Err(message) => {
                let response = self.proxy.response(&request, false, Some(message), None);
                self.to_client(&response).await
            }
        };
        if let Err(error) = sent {
            log(format!("background operation failed: {error}"));
        }
    }

    async fn handle_client_message(self: &Arc<Self>, mut message: Message) -> std::io::Result<()> {
        if message.get("type").and_then(Value::as_str) == Some("request") {
            let command = message
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_string);
            let sequence = message.get("seq").and_then(Value::as_i64);
            if let (Some(sequence), Some(command)) = (sequence, &command) {
                self.state
                    .lock()
                    .unwrap()
                    .client_commands
                    .insert(sequence, command.clone());
            }
            let empty = Message::new();
            let arguments = match message.get("arguments") {
                Some(Value::Object(arguments)) => arguments,
                _ => &empty,
            };
            match command.as_deref() {
                Some("restart") => {
                    self.start_background(self.clone().restart(message));
                    return Ok(());
                }
                Some("variables") => {
                    let reference = arguments.get("variablesReference").and_then(Value::as_i64);
                    let is_locals = reference.is_some_and(|reference| {
                        self.state
                            .lock()
                            .unwrap()
                            .local_references
                            .contains(&reference)
                    });
                    if is_locals {
                        let response = self.proxy.response(
                            &message,
                            true,
                            None,
                            Some(json!({"variables": []})),
                        );
                        return self.to_client(&response).await;
                    }
                }
                Some("launch")
                    if arguments.get("request").and_then(Value::as_str) == Some("attach") =>
                {
                    if let Some(sequence) = sequence {
                        self.state.lock().unwrap().launch_rewrites.insert(sequence);
                    }
                    message.insert("command".into(), json!("attach"));
                }
                _ => {}
            }
        }
        self.to_backend(&message).await
    }

    async fn handle_backend_message(&self, mut message: Message) -> std::io::Result<()> {
        if message.get("type").and_then(Value::as_str) == Some("response") {
            let request_seq = message.get("request_seq").and_then(Value::as_i64);
            let mut state = self.state.lock().unwrap();
            if let Some(pending) = request_seq.and_then(|seq| state.internal_requests.get_mut(&seq))
            {
                if let Some(sender) = pending.take() {
                    let answer = if truthy(message.get("success")) {
                        Ok(message)
                    } else {
                        Err(truthy_str(message.get("message"))
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                let command = message
                                    .get("command")
                                    .and_then(Value::as_str)
                                    .unwrap_or("None");
                                format!("{command} failed")
                            }))
                    };
                    let _ = sender.send(answer);
                }
                return Ok(());
            }

            let remembered = request_seq.and_then(|seq| state.client_commands.remove(&seq));
            let mut command = truthy_str(message.get("command"))
                .map(str::to_string)
                .or(remembered);
            if request_seq.is_some_and(|seq| state.launch_rewrites.remove(&seq)) {
                message.insert("command".into(), json!("launch"));
                command = Some("launch".into());
            }
            if command.as_deref() == Some("scopes") {
                let scopes = message
                    .get("body")
                    .and_then(|body| body.get("scopes"))
                    .and_then(Value::as_array);
                for scope in scopes.into_iter().flatten() {
                    let name = scope.get("name").and_then(Value::as_str).unwrap_or("");
                    let hint = scope.get("presentationHint").and_then(Value::as_str);
                    if hint == Some("locals") || name.to_lowercase() == "locals" {
                        if let Some(reference) =
                            scope.get("variablesReference").and_then(Value::as_i64)
                        {
                            state.local_references.insert(reference);
                        }
                    }
                }
            }
        }
        self.to_client(&message).await
    }
}

async fn pump_client(
    session: Arc<Session>,
    mut reader: OwnedReadHalf,
    initial: Vec<u8>,
) -> std::result::Result<(), SessionError> {
    let mut decoder = DapDecoder::default();
    let mut chunk = initial;
    let mut buffer = vec![0u8; READ_SIZE];
    loop {
        for message in decoder.feed(&chunk)? {
            session.handle_client_message(message).await?;
        }
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        chunk = buffer[..count].to_vec();
    }
}

async fn pump_backend(
    session: Arc<Session>,
    mut reader: OwnedReadHalf,
) -> std::result::Result<(), SessionError> {
    let mut decoder = DapDecoder::default();
    let mut buffer = vec![0u8; READ_SIZE];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        for message in decoder.feed(&buffer[..count])? {
            session.handle_backend_message(message).await?;
        }
    }
}

/// `DapSession.run`: pump both directions until one side closes, then cancel
/// background work and pending internal requests and close the backend.
pub async fn run_session(
    proxy: Arc<ProxyCore>,
    client: TcpStream,
    backend: TcpStream,
    initial: Vec<u8>,
) -> std::result::Result<(), SessionError> {
    let (client_reader, client_writer) = client.into_split();
    let (backend_reader, backend_writer) = backend.into_split();
    let session = Arc::new(Session {
        proxy,
        client_writer: Mutex::new(client_writer),
        backend_writer: Mutex::new(backend_writer),
        state: StdMutex::new(SessionState::default()),
        background: StdMutex::new(Vec::new()),
    });
    let result = tokio::select! {
        result = pump_client(session.clone(), client_reader, initial) => result,
        result = pump_backend(session.clone(), backend_reader) => result,
    };

    let tasks: Vec<JoinHandle<()>> = std::mem::take(&mut *session.background.lock().unwrap());
    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        let _ = task.await;
    }
    session.state.lock().unwrap().internal_requests.clear();
    let _ = session.backend_writer.lock().await.shutdown().await;
    result
}

/// `DapProxy`: owns the adapter process and the listening socket.
pub struct DapProxy {
    config: Config,
    core: Arc<ProxyCore>,
    session_active: AtomicBool,
    exit_code: AtomicI32,
    finished: watch::Sender<bool>,
}

impl DapProxy {
    pub fn new(config: Config, reset: ResetFn) -> Arc<DapProxy> {
        Arc::new(DapProxy {
            config,
            core: ProxyCore::new(reset),
            session_active: AtomicBool::new(false),
            exit_code: AtomicI32::new(0),
            finished: watch::channel(false).0,
        })
    }

    /// `start_adapter`.
    fn start_adapter(&self) -> Result<tokio::process::Child> {
        require_file(&self.config.debug_adapter, "t32debugadapter", true)?;
        if port_open(self.config.dap_backend_port) {
            bail!(
                "internal DAP port {} is already in use",
                self.config.dap_backend_port
            );
        }
        let mut command = tokio::process::Command::new(&self.config.debug_adapter);
        command
            .arg("--port")
            .arg(self.config.dap_backend_port.to_string())
            .args(["--log_to", "stdout"])
            .kill_on_drop(true);
        if std::env::var("T32_DAP_DEBUG").as_deref() == Ok("1") {
            command.args(["--log_level", "debug"]);
        }
        command
            .spawn()
            .map_err(|error| bridge_error!("cannot start DAP proxy: {error}"))
    }

    /// `connect_backend`: retry refused connections for `dap_backend_timeout`.
    async fn connect_backend(&self) -> std::result::Result<TcpStream, String> {
        let port = self.config.dap_backend_port;
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(self.config.dap_backend_timeout);
        loop {
            match TcpStream::connect(("127.0.0.1", port)).await {
                Ok(stream) => return Ok(stream),
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(format!("cannot connect to t32debugadapter on {port}"));
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    /// `handle_client`: only the first connection that sends data becomes the
    /// session; when it ends, the proxy shuts down.
    async fn handle_client(self: Arc<Self>, mut client: TcpStream) {
        let peer = client
            .peer_addr()
            .map(|address| address.to_string())
            .unwrap_or_else(|_| "unknown".into());
        let mut initial = vec![0u8; READ_SIZE];
        let count = match client.read(&mut initial).await {
            Ok(0) | Err(_) => return,
            Ok(count) => count,
        };
        initial.truncate(count);
        if self.session_active.swap(true, Ordering::SeqCst) {
            return;
        }
        let result = match self.connect_backend().await {
            Ok(backend) => {
                log(format!("IDE connected ({peer})"));
                run_session(self.core.clone(), client, backend, initial)
                    .await
                    .map_err(|error| error.to_string())
            }
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            log(format!("session error: {error}"));
            self.exit_code.store(1, Ordering::SeqCst);
        }
        let _ = self.finished.send(true);
    }

    /// `DapProxy.run`: start the adapter, listen, and wait for the adapter to
    /// exit, the session to end, or a shutdown request.
    pub async fn run(self: Arc<Self>, shutdown: impl Future<Output = ()>) -> Result<i32> {
        let mut adapter = self.start_adapter()?;
        let listener = match TcpListener::bind(("127.0.0.1", self.config.dap_port)).await {
            Ok(listener) => listener,
            Err(error) => {
                let _ = adapter.kill().await;
                bail!("cannot start DAP proxy: {error}");
            }
        };
        log(format!(
            "adapter listening on 127.0.0.1:{} (backend {})",
            self.config.dap_port, self.config.dap_backend_port
        ));

        let accepting = {
            let proxy = self.clone();
            tokio::spawn(async move {
                let mut sessions = Vec::new();
                while let Ok((stream, _)) = listener.accept().await {
                    sessions.push(tokio::spawn(proxy.clone().handle_client(stream)));
                }
                sessions
            })
        };

        let mut finished = self.finished.subscribe();
        tokio::select! {
            status = adapter.wait() => {
                if !*self.finished.borrow() {
                    let code = status.map(exit_code).unwrap_or(1);
                    log(format!("t32debugadapter exited ({code})"));
                    self.exit_code.store(if code == 0 { 1 } else { code }, Ordering::SeqCst);
                }
            }
            _ = finished.wait_for(|done| *done) => {}
            _ = shutdown => {}
        }

        accepting.abort();
        terminate(&mut adapter).await;
        Ok(self.exit_code.load(Ordering::SeqCst))
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
    status.code().unwrap_or(1)
}

/// `shutdown`: SIGTERM the adapter, wait 2 s, then kill it.
async fn terminate(adapter: &mut tokio::process::Child) {
    if matches!(adapter.try_wait(), Ok(Some(_))) {
        return;
    }
    #[cfg(unix)]
    if let Some(pid) = adapter.id() {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    if tokio::time::timeout(Duration::from_secs(2), adapter.wait())
        .await
        .is_err()
    {
        let _ = adapter.kill().await;
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) {
            (Ok(mut interrupt), Ok(mut terminate)) => {
                tokio::select! {
                    _ = interrupt.recv() => {}
                    _ = terminate.recv() => {}
                }
            }
            _ => std::future::pending().await,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// `run_proxy`.
pub fn run_proxy(config: &Config) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| bridge_error!("cannot start DAP proxy: {error}"))?;
    let reset_config = config.clone();
    let reset: ResetFn = Arc::new(move || {
        let config = reset_config.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || reset_and_stop(&config))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.0)
        })
    });
    let proxy = DapProxy::new(config.clone(), reset);
    let code = runtime.block_on(proxy.run(shutdown_signal()))?;
    if code != 0 {
        bail!("DAP proxy exited with code {code}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncBufReadExt;

    async fn stream_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (remote, accepted) = tokio::join!(TcpStream::connect(address), listener.accept());
        (accepted.unwrap().0, remote.unwrap())
    }

    fn message(value: Value) -> Message {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    fn frame(value: Value) -> Vec<u8> {
        encode_message(&message(value)).unwrap()
    }

    struct Peer {
        reader: tokio::io::BufReader<OwnedReadHalf>,
        writer: OwnedWriteHalf,
    }

    impl Peer {
        fn new(stream: TcpStream) -> Peer {
            let (reader, writer) = stream.into_split();
            Peer {
                reader: tokio::io::BufReader::new(reader),
                writer,
            }
        }

        async fn send(&mut self, value: Value) {
            self.writer.write_all(&frame(value)).await.unwrap();
        }

        async fn read(&mut self) -> Value {
            let mut length = None;
            loop {
                let mut line = String::new();
                self.reader.read_line(&mut line).await.unwrap();
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = Some(value.trim().parse::<usize>().unwrap());
                    }
                }
            }
            let mut body = vec![0u8; length.expect("missing Content-Length")];
            self.reader.read_exact(&mut body).await.unwrap();
            serde_json::from_slice(&body).unwrap()
        }

        async fn read_timeout(&mut self) -> Option<Value> {
            tokio::time::timeout(Duration::from_millis(50), self.read())
                .await
                .ok()
        }
    }

    struct Harness {
        client: Peer,
        backend: Peer,
        task: JoinHandle<std::result::Result<(), SessionError>>,
    }

    async fn harness(reset: ResetFn, initial: Value) -> Harness {
        let (proxy_client, test_client) = stream_pair().await;
        let (proxy_backend, test_backend) = stream_pair().await;
        let core = ProxyCore::new(reset);
        let task = tokio::spawn(run_session(
            core,
            proxy_client,
            proxy_backend,
            frame(initial),
        ));
        Harness {
            client: Peer::new(test_client),
            backend: Peer::new(test_backend),
            task,
        }
    }

    fn reset_ok(log: Arc<StdMutex<Vec<&'static str>>>) -> ResetFn {
        Arc::new(move || {
            let log = log.clone();
            Box::pin(async move {
                log.lock().unwrap().push("reset");
                Ok(())
            })
        })
    }

    fn no_reset() -> ResetFn {
        Arc::new(|| Box::pin(async { Err("unexpected reset".to_string()) }))
    }

    // test_dap_proxy.py: test_locals_reference_is_intercepted
    #[tokio::test]
    async fn locals_reference_is_intercepted() {
        let mut h = harness(
            no_reset(),
            json!({"seq": 1, "type": "request", "command": "scopes", "arguments": {"frameId": 1}}),
        )
        .await;
        assert_eq!(h.backend.read().await["command"], "scopes");
        h.backend
            .send(json!({
                "seq": 2, "type": "response", "request_seq": 1, "success": true,
                "command": "scopes",
                "body": {"scopes": [
                    {"name": "Locals", "presentationHint": "locals", "variablesReference": 42},
                    {"name": "Registers", "variablesReference": 43}
                ]}
            }))
            .await;
        assert_eq!(h.client.read().await["request_seq"], 1);

        h.client
            .send(json!({"seq": 3, "type": "request", "command": "variables",
                         "arguments": {"variablesReference": 42}}))
            .await;
        let response = h.client.read().await;
        assert_eq!(response["success"], true);
        assert_eq!(response["request_seq"], 3);
        assert_eq!(response["command"], "variables");
        assert_eq!(response["body"], json!({"variables": []}));
        assert_eq!(response["message"], Value::Null);
        assert!(response["seq"].as_i64().unwrap() >= FIRST_SEQUENCE);
        assert!(h.backend.read_timeout().await.is_none());

        // Other references are forwarded.
        h.client
            .send(json!({"seq": 4, "type": "request", "command": "variables",
                         "arguments": {"variablesReference": 43}}))
            .await;
        assert_eq!(h.backend.read().await["seq"], 4);

        drop(h.client);
        tokio::time::timeout(Duration::from_secs(1), h.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn scope_name_locals_matches_without_hint() {
        let mut h = harness(
            no_reset(),
            json!({"seq": 1, "type": "request", "command": "scopes"}),
        )
        .await;
        h.backend.read().await;
        // No "command" in the response: the remembered request command is used.
        h.backend
            .send(
                json!({"seq": 2, "type": "response", "request_seq": 1, "success": true,
                         "body": {"scopes": [{"name": "LOCALS", "variablesReference": 7}]}}),
            )
            .await;
        h.client.read().await;
        h.client
            .send(json!({"seq": 3, "type": "request", "command": "variables",
                         "arguments": {"variablesReference": 7}}))
            .await;
        assert_eq!(h.client.read().await["body"], json!({"variables": []}));
    }

    // test_dap_proxy.py: test_closing_session_cancels_pending_restart
    #[tokio::test]
    async fn closing_session_cancels_pending_restart() {
        let started = Arc::new(tokio::sync::Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        struct SetOnDrop(Arc<AtomicBool>);
        impl Drop for SetOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let reset: ResetFn = {
            let (started, cancelled) = (started.clone(), cancelled.clone());
            Arc::new(move || {
                let (started, guard) = (started.clone(), SetOnDrop(cancelled.clone()));
                Box::pin(async move {
                    let _guard = guard;
                    started.notify_one();
                    std::future::pending::<()>().await;
                    Ok(())
                })
            })
        };
        let h = harness(
            reset,
            json!({"seq": 1, "type": "request", "command": "restart", "arguments": {}}),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .unwrap();
        drop(h.client);
        tokio::time::timeout(Duration::from_secs(1), h.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn restart_resets_then_continues() {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let mut h = harness(
            reset_ok(calls.clone()),
            json!({"seq": 5, "type": "request", "command": "restart", "arguments": {}}),
        )
        .await;
        let internal = h.backend.read().await;
        assert_eq!(internal["command"], "continue");
        assert_eq!(internal["arguments"], json!({"threadId": 0}));
        assert_eq!(internal["type"], "request");
        let internal_seq = internal["seq"].as_i64().unwrap();
        assert!(internal_seq >= FIRST_SEQUENCE);
        assert_eq!(*calls.lock().unwrap(), ["reset"]);
        h.backend
            .send(
                json!({"seq": 9, "type": "response", "request_seq": internal_seq,
                         "success": true, "command": "continue"}),
            )
            .await;
        let response = h.client.read().await;
        assert_eq!(response["request_seq"], 5);
        assert_eq!(response["command"], "restart");
        assert_eq!(response["success"], true);
        let event = h.client.read().await;
        assert_eq!(event["type"], "event");
        assert_eq!(event["event"], "continued");
        assert_eq!(
            event["body"],
            json!({"threadId": 0, "allThreadsContinued": true})
        );
        // The internal response was consumed, not forwarded.
        assert!(h.client.read_timeout().await.is_none());
    }

    #[tokio::test]
    async fn restart_clears_locals_references() {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let mut h = harness(
            reset_ok(calls),
            json!({"seq": 1, "type": "request", "command": "scopes"}),
        )
        .await;
        h.backend.read().await;
        h.backend
            .send(
                json!({"seq": 2, "type": "response", "request_seq": 1, "success": true,
                         "command": "scopes",
                         "body": {"scopes": [{"name": "Locals", "variablesReference": 42}]}}),
            )
            .await;
        h.client.read().await;
        h.client
            .send(json!({"seq": 3, "type": "request", "command": "restart"}))
            .await;
        let internal = h.backend.read().await;
        h.backend
            .send(
                json!({"seq": 4, "type": "response", "request_seq": internal["seq"],
                         "success": true, "command": "continue"}),
            )
            .await;
        h.client.read().await;
        h.client.read().await;
        h.client
            .send(json!({"seq": 5, "type": "request", "command": "variables",
                         "arguments": {"variablesReference": 42}}))
            .await;
        assert_eq!(h.backend.read().await["seq"], 5);
    }

    #[tokio::test]
    async fn restart_failure_is_reported_to_the_client() {
        let reset: ResetFn = Arc::new(|| {
            Box::pin(async { Err("TRACE32 reset failed: target not responding".to_string()) })
        });
        let mut h = harness(
            reset,
            json!({"seq": 5, "type": "request", "command": "restart"}),
        )
        .await;
        let response = h.client.read().await;
        assert_eq!(response["success"], false);
        assert_eq!(response["request_seq"], 5);
        assert_eq!(
            response["message"],
            "TRACE32 reset failed: target not responding"
        );
        assert_eq!(response["body"], Value::Null);
        assert!(h.backend.read_timeout().await.is_none());
    }

    #[tokio::test]
    async fn failed_internal_continue_is_reported() {
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let mut h = harness(
            reset_ok(calls),
            json!({"seq": 5, "type": "request", "command": "restart"}),
        )
        .await;
        let internal = h.backend.read().await;
        h.backend
            .send(
                json!({"seq": 9, "type": "response", "request_seq": internal["seq"],
                         "success": false, "command": "continue"}),
            )
            .await;
        let response = h.client.read().await;
        assert_eq!(response["success"], false);
        assert_eq!(response["message"], "continue failed");
    }

    #[tokio::test]
    async fn launch_with_attach_request_is_rewritten() {
        let mut h = harness(
            no_reset(),
            json!({"seq": 2, "type": "request", "command": "launch",
                   "arguments": {"type": "node", "request": "attach", "trace32Port": 20000}}),
        )
        .await;
        let forwarded = h.backend.read().await;
        assert_eq!(forwarded["command"], "attach");
        assert_eq!(forwarded["arguments"]["trace32Port"], 20000);
        h.backend
            .send(
                json!({"seq": 3, "type": "response", "request_seq": 2, "success": true,
                         "command": "attach"}),
            )
            .await;
        let response = h.client.read().await;
        assert_eq!(response["command"], "launch");
        assert_eq!(response["request_seq"], 2);
    }

    #[tokio::test]
    async fn plain_launch_is_forwarded_unchanged() {
        let mut h = harness(
            no_reset(),
            json!({"seq": 2, "type": "request", "command": "launch", "arguments": {}}),
        )
        .await;
        assert_eq!(h.backend.read().await["command"], "launch");
    }

    #[tokio::test]
    async fn messages_keep_their_fields_and_order() {
        let mut h = harness(
            no_reset(),
            json!({"seq": 1, "type": "request", "command": "initialize",
                   "arguments": {"clientID": "vscode", "adapterID": "node", "linesStartAt1": true}}),
        )
        .await;
        let forwarded = h.backend.read().await;
        assert_eq!(
            serde_json::to_string(&forwarded).unwrap(),
            r#"{"seq":1,"type":"request","command":"initialize","arguments":{"clientID":"vscode","adapterID":"node","linesStartAt1":true}}"#
        );
        h.backend
            .send(json!({"seq": 1, "type": "event", "event": "initialized"}))
            .await;
        assert_eq!(h.client.read().await["event"], "initialized");
    }

    #[tokio::test]
    async fn invalid_client_framing_ends_the_session_with_an_error() {
        let (proxy_client, mut test_client) = stream_pair().await;
        let (proxy_backend, _test_backend) = stream_pair().await;
        let task = tokio::spawn(run_session(
            ProxyCore::new(no_reset()),
            proxy_client,
            proxy_backend,
            Vec::new(),
        ));
        test_client.write_all(b"Other: 1\r\n\r\n{}").await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.to_string(), "DAP message has no Content-Length");
    }

    #[tokio::test]
    async fn backend_that_never_listens_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::target::tests::make_config(dir.path());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        config.dap_backend_port = listener.local_addr().unwrap().port();
        drop(listener);
        config.dap_backend_timeout = 1;
        let proxy = DapProxy::new(config.clone(), no_reset());
        let started = tokio::time::Instant::now();
        let error = proxy.connect_backend().await.unwrap_err();
        assert_eq!(
            error,
            format!(
                "cannot connect to t32debugadapter on {}",
                config.dap_backend_port
            )
        );
        assert!(started.elapsed() >= Duration::from_millis(900));
    }
}
