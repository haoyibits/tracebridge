//! Smoke test against a running PowerView.
//!
//! ```text
//! cargo run -p t32rcl --example smoke -- [--port 20000] [--address E:0x20000000] [--length 32]
//! ```
//!
//! Connects, prints a line in PowerView, reads `SYStem.Up()` and `STATE.RUN()`
//! and dumps memory. tools/smoke.py does the same through the Python library;
//! both print the same lines, so their outputs can be compared with `diff`.

use std::time::Duration;

use t32rcl::{Address, Debugger};

fn main() {
    let mut port = 20000u16;
    let mut address = "E:0x20000000".to_string();
    let mut length = 32usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().expect("option value");
        match arg.as_str() {
            "--port" => port = value.parse().expect("port"),
            "--address" => address = value,
            "--length" => length = value.parse().expect("length"),
            _ => panic!("unknown option {arg}"),
        }
    }

    let run = || -> t32rcl::Result<()> {
        let mut debugger = Debugger::connect("localhost", port, Duration::from_secs(5))?;
        debugger.print("t32rcl smoke test")?;
        println!("system_up {}", debugger.system_up()?);
        println!("state_run {}", debugger.state_run()?);
        let data = debugger.memory_read(&Address::parse(&address)?, length)?;
        println!("read {address} {}", hex(&data));
        Ok(())
    };
    if let Err(error) = run() {
        eprintln!("smoke: {error}");
        std::process::exit(1);
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}
