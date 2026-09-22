//! TCP forwarder that records RCL traffic as a replay fixture.
//!
//! ```text
//! cargo run -p t32rcl --example capture_proxy -- \
//!     [--listen 20001] [--upstream 127.0.0.1:20000] --out crates/t32rcl/tests/fixtures/<name>
//! ```
//!
//! Accepts one client, forwards it to PowerView and writes `client.bin` (client
//! to PowerView) and `server.bin` (PowerView to client) into the output
//! directory. Exits when either side closes the connection.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;

fn pump(mut from: TcpStream, mut to: TcpStream, mut log: File) -> io::Result<u64> {
    let mut buffer = [0u8; 65536];
    let mut total = 0;
    loop {
        let count = from.read(&mut buffer)?;
        if count == 0 {
            let _ = to.shutdown(Shutdown::Write);
            return Ok(total);
        }
        log.write_all(&buffer[..count])?;
        to.write_all(&buffer[..count])?;
        total += count as u64;
    }
}

fn main() -> io::Result<()> {
    let mut listen = "20001".to_string();
    let mut upstream = "127.0.0.1:20000".to_string();
    let mut out: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next();
        match (arg.as_str(), value) {
            ("--listen", Some(value)) => listen = value,
            ("--upstream", Some(value)) => upstream = value,
            ("--out", Some(value)) => out = Some(value.into()),
            _ => {
                eprintln!("usage: capture_proxy [--listen PORT] [--upstream HOST:PORT] --out DIR");
                std::process::exit(2);
            }
        }
    }
    let Some(out) = out else {
        eprintln!("--out is required");
        std::process::exit(2);
    };
    fs::create_dir_all(&out)?;

    let listener = TcpListener::bind(("127.0.0.1", listen.parse::<u16>().expect("port")))?;
    eprintln!(
        "capture_proxy: listening on {}, forwarding to {upstream}",
        listener.local_addr()?
    );
    let (client, peer) = listener.accept()?;
    let server = TcpStream::connect(&upstream)?;
    eprintln!("capture_proxy: {peer} connected");

    let to_server = {
        let (client, server, log) = (
            client.try_clone()?,
            server.try_clone()?,
            File::create(out.join("client.bin"))?,
        );
        thread::spawn(move || pump(client, server, log))
    };
    let to_client = pump(server, client, File::create(out.join("server.bin"))?);
    let sent = to_server.join().expect("forwarder thread")?;
    eprintln!(
        "capture_proxy: recorded {sent} client bytes and {} server bytes in {}",
        to_client?,
        out.display()
    );
    Ok(())
}
