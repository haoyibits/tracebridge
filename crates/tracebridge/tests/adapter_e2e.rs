//! `tracebridge adapter` end to end: the binary, a fake PowerView (RCL) and a
//! fake t32debugadapter (Python script; skipped when python3 is missing).

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::{FakeRcl, Project, command_in, free_port};
use serde_json::{Value, json};

fn python_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn install_fake_adapter(project: &Project) -> std::path::PathBuf {
    let path = project.root().join("fake_t32debugadapter.py");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fake_t32debugadapter.py"
        ),
        &path,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

struct Client {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    seq: i64,
}

impl Client {
    fn connect(port: u16) -> Client {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        Client {
            stream,
            reader,
            seq: 0,
        }
    }

    fn request(&mut self, command: &str, arguments: Value) -> i64 {
        self.seq += 1;
        let body = json!({"seq": self.seq, "type": "request", "command": command,
                          "arguments": arguments})
        .to_string();
        write!(self.stream, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.seq
    }

    fn read(&mut self) -> Value {
        let mut length = 0;
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        self.reader.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }
}

/// Start `tracebridge adapter` and wait for its ready line.
fn start_adapter(project: &Project) -> (Child, mpsc::Receiver<String>) {
    let mut child = command_in(&project.root(), &["adapter"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
    loop {
        match receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(line) if line.starts_with("[tracebridge] adapter listening on 127.0.0.1:") => {
                return (child, receiver);
            }
            Ok(_) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("adapter did not become ready");
            }
        }
    }
}

#[test]
fn full_session_through_the_proxy() {
    if !python_available() {
        eprintln!("skipped: python3 not available");
        return;
    }
    let rcl = FakeRcl::start();
    let (dap_port, backend_port) = (free_port(), free_port());
    let project = Project::new(
        rcl.port,
        &format!(
            "dap_port = {dap_port}\ndap_backend_port = {backend_port}\ndap_backend_timeout = 10\n"
        ),
    );
    let adapter = install_fake_adapter(&project);
    let toml = project.root().join("trace32.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(
        &toml,
        text.replace(
            "[trace32]\n",
            &format!("[trace32]\ndebug_adapter = \"{}\"\n", adapter.display()),
        ),
    )
    .unwrap();

    let (mut child, _lines) = start_adapter(&project);
    let mut client = Client::connect(dap_port);

    let seq = client.request("initialize", json!({"adapterID": "node"}));
    assert_eq!(client.read()["request_seq"], seq);

    // RustRover (LSP4IJ) launch mode is turned into an attach.
    let seq = client.request("launch", json!({"type": "node", "request": "attach"}));
    let response = client.read();
    assert_eq!(response["request_seq"], seq);
    assert_eq!(response["command"], "launch");

    client.request("scopes", json!({"frameId": 1}));
    assert_eq!(client.read()["body"]["scopes"][0]["variablesReference"], 42);
    client.request("variables", json!({"variablesReference": 42}));
    assert_eq!(client.read()["body"], json!({"variables": []}));
    client.request("variables", json!({"variablesReference": 43}));
    assert_eq!(client.read()["body"]["variables"][0]["name"], "r0");

    let seq = client.request("restart", json!({}));
    let response = client.read();
    assert_eq!(response["request_seq"], seq);
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(client.read()["event"], "continued");
    assert_eq!(rcl.state().commands(), ["Break", "SYStem.Mode Up"]);

    drop(client);
    let status = wait(&mut child, Duration::from_secs(10));
    assert_eq!(status, Some(0));
}

fn wait(child: &mut Child, limit: Duration) -> Option<i32> {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    panic!("adapter did not exit");
}

#[test]
fn adapter_requires_powerview() {
    let project = Project::new(free_port(), "");
    let output = project.run(&["adapter"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no PowerView on RCL port") && stderr.contains("'tracebridge open'"),
        "{stderr}"
    );
}

#[test]
fn adapter_already_listening_is_not_an_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dap_port = listener.local_addr().unwrap().port();
    let project = Project::new(free_port(), &format!("dap_port = {dap_port}\n"));
    let output = project.run(&["adapter"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&format!("debug adapter already listening on {dap_port}"))
    );
}

#[test]
fn missing_debug_adapter_is_reported() {
    let rcl = FakeRcl::start();
    let project = Project::new(rcl.port, &format!("dap_port = {}\n", free_port()));
    let output = project.run(&["adapter"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .starts_with("tracebridge: t32debugadapter not found: ")
    );
}
