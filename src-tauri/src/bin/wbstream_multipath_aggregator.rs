use app_lib::wbstream_multipath::{StreamReassembler, TransportRecordDecoder};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn main() {
    if let Err(e) = run() {
        eprintln!("wbstream_multipath_aggregator: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[1] == "--listen" {
        return run_tcp_once(&args[2]);
    }
    if args.len() == 3 && args[1] == "--serve" {
        return run_tcp_server(&args[2]);
    }
    if args.len() == 5 && args[1] == "--serve-multipath" && args[3] == "--paths" {
        let path_count = parse_path_count(&args[4])?;
        return run_tcp_multipath_once(&args[2], path_count);
    }
    if args.len() != 1 {
        return Err(
            "usage: wbstream_multipath_aggregator [--listen 127.0.0.1:PORT|--serve 127.0.0.1:PORT|--serve-multipath 127.0.0.1:PORT --paths N]"
                .to_string(),
        );
    }

    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|e| format!("read stdin: {}", e))?;
    let output = reassemble_records(&input)?;

    std::io::stdout()
        .write_all(&output)
        .map_err(|e| format!("write stdout: {}", e))?;
    Ok(())
}

fn run_tcp_once(addr: &str) -> Result<(), String> {
    let listener = TcpListener::bind(addr).map_err(|e| format!("listen {}: {}", addr, e))?;
    eprintln!(
        "wbstream_multipath_aggregator listening {}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let (mut stream, _) = listener.accept().map_err(|e| format!("accept: {}", e))?;
    handle_stream(&mut stream)
}

fn run_tcp_server(addr: &str) -> Result<(), String> {
    let listener = TcpListener::bind(addr).map_err(|e| format!("listen {}: {}", addr, e))?;
    eprintln!(
        "wbstream_multipath_aggregator serving {}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                if let Err(e) = handle_stream(&mut stream) {
                    eprintln!("wbstream_multipath_aggregator connection error: {}", e);
                }
            }
            Err(e) => eprintln!("wbstream_multipath_aggregator accept error: {}", e),
        }
    }
    Ok(())
}

fn run_tcp_multipath_once(addr: &str, path_count: usize) -> Result<(), String> {
    let listener = TcpListener::bind(addr).map_err(|e| format!("listen {}: {}", addr, e))?;
    eprintln!(
        "wbstream_multipath_aggregator serving one multipath stream on {} paths={}",
        listener.local_addr().map_err(|e| e.to_string())?,
        path_count
    );

    let (tx, rx) = mpsc::channel();
    let mut streams = Vec::new();
    for _ in 0..path_count {
        let (stream, _) = listener.accept().map_err(|e| format!("accept: {}", e))?;
        let reader = stream
            .try_clone()
            .map_err(|e| format!("clone multipath stream: {}", e))?;
        let tx = tx.clone();
        thread::spawn(move || read_frames_until_eof(reader, tx));
        streams.push(stream);
    }
    drop(tx);

    let mut reassembler = StreamReassembler::new(1024 * 1024);
    let mut output = Vec::new();
    for frame in rx {
        output.extend_from_slice(&reassembler.push(frame)?.bytes);
        if reassembler.is_complete() {
            break;
        }
    }
    if !reassembler.is_complete() {
        return Err("multipath stream did not complete".to_string());
    }
    for (index, stream) in streams.iter_mut().enumerate() {
        if index == 0 {
            stream
                .write_all(&output)
                .map_err(|e| format!("write multipath response: {}", e))?;
        }
        let _ = stream.shutdown(std::net::Shutdown::Write);
    }
    Ok(())
}

fn read_frames_until_eof(
    mut stream: TcpStream,
    tx: mpsc::Sender<app_lib::wbstream_multipath::MultipathFrame>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut decoder = TransportRecordDecoder::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => match decoder.push_bytes(&buf[..n]) {
                Ok(frames) => {
                    for frame in frames {
                        if tx.send(frame).is_err() {
                            return;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("wbstream_multipath_aggregator path decode error: {}", e);
                    return;
                }
            },
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                break;
            }
            Err(e) => {
                eprintln!("wbstream_multipath_aggregator path read error: {}", e);
                break;
            }
        }
    }
}

fn handle_stream(stream: &mut TcpStream) -> Result<(), String> {
    let input = read_request_or_stream(stream)?;
    if is_health_request(&input) {
        return write_health_response(stream);
    }
    let output = reassemble_records(&input)?;
    stream
        .write_all(&output)
        .map_err(|e| format!("write tcp stream: {}", e))?;
    stream
        .flush()
        .map_err(|e| format!("flush tcp stream: {}", e))?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    std::thread::sleep(Duration::from_millis(10));
    Ok(())
}

fn parse_path_count(value: &str) -> Result<usize, String> {
    let path_count = value
        .parse::<usize>()
        .map_err(|e| format!("parse paths: {}", e))?;
    if path_count == 0 {
        return Err("paths must be positive".to_string());
    }
    Ok(path_count)
}

fn read_request_or_stream(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("set read timeout: {}", e))?;
    let mut input = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return Ok(input),
            Ok(n) => {
                input.extend_from_slice(&buf[..n]);
                if is_health_request(&input) {
                    return Ok(input);
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                return Err("read tcp stream timeout".to_string());
            }
            Err(e) => return Err(format!("read tcp stream: {}", e)),
        }
    }
}

fn is_health_request(input: &[u8]) -> bool {
    input.starts_with(b"GET /health ")
        || input == b"HEALTH\n"
        || input == b"VERSION\n"
        || input == b"PING\n"
}

fn write_health_response(stream: &mut TcpStream) -> Result<(), String> {
    let body = format!(
        "{{\"ok\":true,\"service\":\"wbstream_multipath_aggregator\",\"version\":\"{}\"}}\n",
        env!("CARGO_PKG_VERSION")
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|e| format!("write health response: {}", e))
}

fn reassemble_records(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = TransportRecordDecoder::new();
    let mut reassembler = StreamReassembler::new(1024 * 1024);
    let mut output = Vec::new();
    for frame in decoder.push_bytes(input)? {
        output.extend_from_slice(&reassembler.push(frame)?.bytes);
    }
    if decoder.buffered_bytes() != 0 {
        return Err("incomplete multipath transport record".to_string());
    }
    if !reassembler.is_complete() {
        return Err("multipath stream did not complete".to_string());
    }
    Ok(output)
}
