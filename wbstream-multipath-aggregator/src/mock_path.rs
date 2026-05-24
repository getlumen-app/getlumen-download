use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread::sleep;
use std::time::Duration;

fn main() {
    if let Err(e) = run() {
        eprintln!("wbstream_mock_path: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 9
        || args[1] != "--listen"
        || args[3] != "--upstream"
        || args[5] != "--delay-ms"
        || args[7] != "--fragment-size"
    {
        return Err(
            "usage: wbstream_mock_path --listen 127.0.0.1:PORT --upstream 127.0.0.1:PORT --delay-ms N --fragment-size N"
                .to_string(),
        );
    }

    let delay_ms = parse_u64(&args[6], "delay-ms")?;
    let fragment_size = parse_usize(&args[8], "fragment-size")?;
    run_once(&args[2], &args[4], delay_ms, fragment_size)
}

fn run_once(
    listen_addr: &str,
    upstream_addr: &str,
    delay_ms: u64,
    fragment_size: usize,
) -> Result<(), String> {
    let listener =
        TcpListener::bind(listen_addr).map_err(|e| format!("listen {}: {}", listen_addr, e))?;
    eprintln!(
        "wbstream_mock_path listening {} -> {} delay_ms={} fragment_size={}",
        listener.local_addr().map_err(|e| e.to_string())?,
        upstream_addr,
        delay_ms,
        fragment_size
    );
    let (mut inbound, _) = listener.accept().map_err(|e| format!("accept: {}", e))?;
    let mut payload = Vec::new();
    inbound
        .read_to_end(&mut payload)
        .map_err(|e| format!("read inbound: {}", e))?;

    let mut upstream = TcpStream::connect(upstream_addr)
        .map_err(|e| format!("connect upstream {}: {}", upstream_addr, e))?;
    for chunk in payload.chunks(fragment_size) {
        if delay_ms > 0 {
            sleep(Duration::from_millis(delay_ms));
        }
        if let Err(e) = upstream.write_all(chunk) {
            if is_closed_pipe(&e) {
                break;
            }
            return Err(format!("write upstream: {}", e));
        }
    }
    match upstream.shutdown(Shutdown::Write) {
        Ok(()) => {}
        Err(e) if is_closed_pipe(&e) => {}
        Err(e) => return Err(format!("shutdown upstream write: {}", e)),
    }

    let mut response = Vec::new();
    match upstream.read_to_end(&mut response) {
        Ok(_) => {}
        Err(e) if is_closed_pipe(&e) => {}
        Err(e) => return Err(format!("read upstream: {}", e)),
    }
    inbound
        .write_all(&response)
        .map_err(|e| format!("write inbound: {}", e))?;
    Ok(())
}

fn is_closed_pipe(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::NotConnected
    )
}

fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|e| format!("parse {}: {}", name, e))
}

fn parse_usize(value: &str, name: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|e| format!("parse {}: {}", name, e))?;
    if parsed == 0 {
        return Err(format!("{} must be positive", name));
    }
    Ok(parsed)
}
