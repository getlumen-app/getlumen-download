#[path = "../../src-tauri/src/wbstream_multipath.rs"]
mod wbstream_multipath;

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;
use wbstream_multipath::{
    encode_transport_record, PathScheduler, PathScore, StreamReassembler, StreamSplitter,
    TransportRecordDecoder,
};

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
        return run_tcp_multipath_once(&args[2], path_count, MultipathMode::Echo);
    }
    if args.len() == 5 && args[1] == "--serve-multipath-loop" && args[3] == "--paths" {
        let path_count = parse_path_count(&args[4])?;
        return run_tcp_multipath_server(&args[2], path_count, MultipathMode::Echo);
    }
    if args.len() == 5 && args[1] == "--serve-proxy-multipath" && args[3] == "--paths" {
        let path_count = parse_path_count(&args[4])?;
        return run_tcp_multipath_once(&args[2], path_count, MultipathMode::Proxy);
    }
    if args.len() == 5 && args[1] == "--serve-proxy-multipath-loop" && args[3] == "--paths" {
        let path_count = parse_path_count(&args[4])?;
        return run_tcp_multipath_server(&args[2], path_count, MultipathMode::Proxy);
    }
    if args.len() == 5 && args[1] == "--serve-proxy-duplex" && args[3] == "--paths" {
        let path_count = parse_path_count(&args[4])?;
        return run_tcp_duplex_once(&args[2], path_count);
    }
    if args.len() == 5 && args[1] == "--serve-proxy-duplex-loop" && args[3] == "--paths" {
        let path_count = parse_path_count(&args[4])?;
        return run_tcp_duplex_server(&args[2], path_count);
    }
    if args.len() != 1 {
        return Err(
            "usage: wbstream_multipath_aggregator [--listen 127.0.0.1:PORT|--serve 127.0.0.1:PORT|--serve-multipath 127.0.0.1:PORT --paths N|--serve-multipath-loop 127.0.0.1:PORT --paths N|--serve-proxy-multipath 127.0.0.1:PORT --paths N|--serve-proxy-multipath-loop 127.0.0.1:PORT --paths N|--serve-proxy-duplex 127.0.0.1:PORT --paths N|--serve-proxy-duplex-loop 127.0.0.1:PORT --paths N]"
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

fn run_tcp_duplex_once(addr: &str, path_count: usize) -> Result<(), String> {
    let listener = TcpListener::bind(addr).map_err(|e| format!("listen {}: {}", addr, e))?;
    eprintln!(
        "wbstream_multipath_aggregator serving duplex proxy on {} paths={}",
        listener.local_addr().map_err(|e| e.to_string())?,
        path_count
    );
    let (frame_tx, frame_rx) = mpsc::channel();
    let mut streams = Vec::new();
    for _ in 0..path_count {
        let (stream, _) = listener.accept().map_err(|e| format!("accept: {}", e))?;
        let reader = stream
            .try_clone()
            .map_err(|e| format!("clone duplex stream: {}", e))?;
        let tx = frame_tx.clone();
        thread::spawn(move || read_frames_until_eof(reader, tx));
        streams.push(stream);
    }
    drop(frame_tx);
    let response_writers = build_path_writers(&streams)?;
    let response_paths = path_scores(path_count)?;
    handle_duplex_proxy_frames(frame_rx, response_writers, response_paths)
}

fn run_tcp_duplex_server(addr: &str, path_count: usize) -> Result<(), String> {
    let listener = TcpListener::bind(addr).map_err(|e| format!("listen {}: {}", addr, e))?;
    eprintln!(
        "wbstream_multipath_aggregator serving duplex proxy loop on {} paths={}",
        listener.local_addr().map_err(|e| e.to_string())?,
        path_count
    );
    loop {
        let (frame_tx, frame_rx) = mpsc::channel();
        let mut streams = Vec::new();
        for _ in 0..path_count {
            let (stream, _) = listener.accept().map_err(|e| format!("accept: {}", e))?;
            let reader = stream
                .try_clone()
                .map_err(|e| format!("clone duplex stream: {}", e))?;
            let tx = frame_tx.clone();
            thread::spawn(move || read_frames_until_eof(reader, tx));
            streams.push(stream);
        }
        drop(frame_tx);
        let response_writers = build_path_writers(&streams)?;
        let response_paths = path_scores(path_count)?;
        if let Err(e) = handle_duplex_proxy_frames(frame_rx, response_writers, response_paths) {
            eprintln!(
                "wbstream_multipath_aggregator duplex connection error: {}",
                e
            );
        }
    }
}

fn handle_duplex_proxy_frames(
    frame_rx: mpsc::Receiver<wbstream_multipath::MultipathFrame>,
    response_writers: Arc<HashMap<u8, Arc<Mutex<TcpStream>>>>,
    response_paths: Vec<PathScore>,
) -> Result<(), String> {
    let mut request_reassembler = StreamReassembler::new(1024 * 1024);
    let mut target: Option<TcpStream> = None;
    let mut prelude_buffer = Vec::new();
    let mut response_started = false;
    for frame in frame_rx {
        if frame.stream_id != 1 {
            continue;
        }
        let frame_session_id = frame.session_id;
        let output = request_reassembler.push(frame)?;
        if !output.bytes.is_empty() {
            if let Some(target) = target.as_mut() {
                if let Err(e) = target.write_all(&output.bytes) {
                    if is_closed_pipe(&e) {
                        break;
                    }
                    return Err(format!("write duplex target: {}", e));
                }
            } else {
                prelude_buffer.extend_from_slice(&output.bytes);
                if let Some((target_addr, remaining)) = try_decode_proxy_prelude(&prelude_buffer)? {
                    let mut connected = TcpStream::connect(&target_addr)
                        .map_err(|e| format!("connect duplex target {}: {}", target_addr, e))?;
                    if !remaining.is_empty() {
                        if let Err(e) = connected.write_all(&remaining) {
                            if is_closed_pipe(&e) {
                                break;
                            }
                            return Err(format!("write initial duplex target: {}", e));
                        }
                    }
                    let mut response_reader = connected
                        .try_clone()
                        .map_err(|e| format!("clone duplex target reader: {}", e))?;
                    let writers = response_writers.clone();
                    let paths = response_paths.clone();
                    thread::spawn(move || {
                        let _ = stream_target_response_as_frames(
                            &mut response_reader,
                            writers,
                            paths,
                            frame_session_id,
                        );
                    });
                    target = Some(connected);
                    response_started = true;
                }
            }
        }
        if output.complete {
            if let Some(target) = target.as_mut() {
                let _ = target.shutdown(Shutdown::Write);
            }
            break;
        }
    }
    if !response_started {
        return Err("duplex proxy never received a complete target prelude".to_string());
    }
    Ok(())
}

fn build_path_writers(
    streams: &[TcpStream],
) -> Result<Arc<HashMap<u8, Arc<Mutex<TcpStream>>>>, String> {
    let mut writers = HashMap::new();
    for (index, stream) in streams.iter().enumerate() {
        let path_id = index as u8 + 1;
        writers.insert(
            path_id,
            Arc::new(Mutex::new(
                stream
                    .try_clone()
                    .map_err(|e| format!("clone duplex response stream {}: {}", path_id, e))?,
            )),
        );
    }
    Ok(Arc::new(writers))
}

fn path_scores(path_count: usize) -> Result<Vec<PathScore>, String> {
    if path_count > u8::MAX as usize {
        return Err("at most 255 duplex paths are supported".to_string());
    }
    Ok((0..path_count)
        .map(|index| PathScore {
            path_id: index as u8 + 1,
            rtt_ms: 10 + index as u32 * 10,
            inflight_bytes: 0,
            healthy: true,
        })
        .collect())
}

fn is_closed_pipe(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::NotConnected
    )
}

fn stream_target_response_as_frames(
    target: &mut TcpStream,
    writers: Arc<HashMap<u8, Arc<Mutex<TcpStream>>>>,
    response_paths: Vec<PathScore>,
    session_id: u64,
) -> Result<(), String> {
    let mut splitter = StreamSplitter::new(session_id, 2, 1024)?;
    let mut scheduler = PathScheduler::new(response_paths)?;
    let mut buf = [0u8; 4096];
    loop {
        match target.read(&mut buf) {
            Ok(0) => {
                write_response_frames(&writers, &mut splitter, &mut scheduler, b"", true)?;
                break;
            }
            Ok(n) => {
                write_response_frames(&writers, &mut splitter, &mut scheduler, &buf[..n], false)?
            }
            Err(e) => return Err(format!("read duplex target response: {}", e)),
        }
    }
    Ok(())
}

fn write_response_frames(
    writers: &Arc<HashMap<u8, Arc<Mutex<TcpStream>>>>,
    splitter: &mut StreamSplitter,
    scheduler: &mut PathScheduler,
    bytes: &[u8],
    finish: bool,
) -> Result<(), String> {
    let frames = splitter.split(bytes, finish, scheduler)?;
    for frame in frames {
        let writer = writers
            .get(&frame.path_id)
            .ok_or_else(|| format!("missing duplex response writer for path {}", frame.path_id))?;
        let mut writer = writer
            .lock()
            .map_err(|_| "duplex response writer lock poisoned".to_string())?;
        writer
            .write_all(&encode_transport_record(&frame)?)
            .map_err(|e| format!("write duplex response frame: {}", e))?;
        writer
            .flush()
            .map_err(|e| format!("flush duplex response frame: {}", e))?;
    }
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

#[derive(Clone, Copy)]
enum MultipathMode {
    Echo,
    Proxy,
}

fn run_tcp_multipath_once(
    addr: &str,
    path_count: usize,
    mode: MultipathMode,
) -> Result<(), String> {
    let listener = TcpListener::bind(addr).map_err(|e| format!("listen {}: {}", addr, e))?;
    eprintln!(
        "wbstream_multipath_aggregator serving one multipath stream on {} paths={} mode={}",
        listener.local_addr().map_err(|e| e.to_string())?,
        path_count,
        match mode {
            MultipathMode::Echo => "echo",
            MultipathMode::Proxy => "proxy",
        }
    );

    handle_multipath_connection_group(&listener, path_count, mode)
}

fn run_tcp_multipath_server(
    addr: &str,
    path_count: usize,
    mode: MultipathMode,
) -> Result<(), String> {
    let listener = TcpListener::bind(addr).map_err(|e| format!("listen {}: {}", addr, e))?;
    eprintln!(
        "wbstream_multipath_aggregator serving multipath loop on {} paths={} mode={}",
        listener.local_addr().map_err(|e| e.to_string())?,
        path_count,
        match mode {
            MultipathMode::Echo => "echo",
            MultipathMode::Proxy => "proxy",
        }
    );
    loop {
        if let Err(e) = handle_multipath_connection_group(&listener, path_count, mode) {
            eprintln!(
                "wbstream_multipath_aggregator multipath connection error: {}",
                e
            );
        }
    }
}

fn handle_multipath_connection_group(
    listener: &TcpListener,
    path_count: usize,
    mode: MultipathMode,
) -> Result<(), String> {
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
    let response = match mode {
        MultipathMode::Echo => output,
        MultipathMode::Proxy => proxy_reassembled_request(&output)?,
    };
    for (index, stream) in streams.iter_mut().enumerate() {
        if index == 0 {
            stream
                .write_all(&response)
                .map_err(|e| format!("write multipath response: {}", e))?;
        }
        let _ = stream.shutdown(Shutdown::Write);
    }
    Ok(())
}

fn read_frames_until_eof(
    mut stream: TcpStream,
    tx: mpsc::Sender<wbstream_multipath::MultipathFrame>,
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

fn proxy_reassembled_request(input: &[u8]) -> Result<Vec<u8>, String> {
    let (target_addr, payload) = decode_proxy_request(input)?;
    let mut target = TcpStream::connect(&target_addr)
        .map_err(|e| format!("connect proxy target {}: {}", target_addr, e))?;
    target
        .write_all(payload)
        .map_err(|e| format!("write proxy target {}: {}", target_addr, e))?;
    target
        .shutdown(Shutdown::Write)
        .map_err(|e| format!("shutdown proxy target {} write: {}", target_addr, e))?;
    let mut response = Vec::new();
    target
        .read_to_end(&mut response)
        .map_err(|e| format!("read proxy target {}: {}", target_addr, e))?;
    Ok(response)
}

fn decode_proxy_request(input: &[u8]) -> Result<(String, &[u8]), String> {
    const PREFIX: &[u8] = b"WBSMPROXY/1\nTARGET ";
    if !input.starts_with(PREFIX) {
        return Err("proxy request missing WBSMPROXY prelude".to_string());
    }
    let rest = &input[PREFIX.len()..];
    let separator = rest
        .windows(2)
        .position(|window| window == b"\n\n")
        .ok_or_else(|| "proxy request missing header terminator".to_string())?;
    let target = std::str::from_utf8(&rest[..separator])
        .map_err(|e| format!("proxy target is not utf8: {}", e))?;
    if target.is_empty() || target.contains('\n') {
        return Err("proxy target is invalid".to_string());
    }
    Ok((target.to_string(), &rest[separator + 2..]))
}

fn try_decode_proxy_prelude(input: &[u8]) -> Result<Option<(String, Vec<u8>)>, String> {
    const PREFIX: &[u8] = b"WBSMPROXY/1\nTARGET ";
    if input.len() < PREFIX.len() {
        return Ok(None);
    }
    if !input.starts_with(PREFIX) {
        return Err("proxy request missing WBSMPROXY prelude".to_string());
    }
    let rest = &input[PREFIX.len()..];
    let Some(separator) = rest.windows(2).position(|window| window == b"\n\n") else {
        return Ok(None);
    };
    let target = std::str::from_utf8(&rest[..separator])
        .map_err(|e| format!("proxy target is not utf8: {}", e))?;
    if target.is_empty() || target.contains('\n') {
        return Err("proxy target is invalid".to_string());
    }
    Ok(Some((target.to_string(), rest[separator + 2..].to_vec())))
}
