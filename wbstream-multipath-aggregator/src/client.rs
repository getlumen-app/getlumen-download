#[path = "../../src-tauri/src/wbstream_multipath.rs"]
mod wbstream_multipath;

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, sleep};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use wbstream_multipath::{
    encode_transport_record, PathScheduler, PathScore, StreamReassembler, StreamSplitter,
    TransportRecordDecoder,
};

const DEFAULT_CHUNK_SIZE: usize = 16;

fn main() {
    if let Err(e) = run() {
        eprintln!("wbstream_multipath_client: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 5 && args[1] == "--listen" && args[3] == "--aggregator" {
        return run_client_once(&args[2], vec![PathEndpoint::Direct(args[4].clone())]);
    }
    if args.len() == 5 && args[1] == "--listen" && args[3] == "--aggregators" {
        let aggregators = parse_aggregators(&args[4])?;
        return run_client_once(
            &args[2],
            aggregators.into_iter().map(PathEndpoint::Direct).collect(),
        );
    }
    if args.len() == 7
        && args[1] == "--listen"
        && args[3] == "--socks-aggregators"
        && args[5] == "--target"
    {
        let socks_addrs = parse_aggregators(&args[4])?;
        let endpoints = socks_addrs
            .into_iter()
            .map(|socks_addr| PathEndpoint::Socks {
                socks_addr,
                target_addr: args[6].clone(),
            })
            .collect();
        return run_client_once(&args[2], endpoints);
    }
    if args.len() == 7
        && args[1] == "--socks-listen"
        && args[3] == "--socks-aggregators"
        && args[5] == "--aggregator-target"
    {
        let socks_addrs = parse_aggregators(&args[4])?;
        let endpoints = socks_addrs
            .into_iter()
            .map(|socks_addr| PathEndpoint::Socks {
                socks_addr,
                target_addr: args[6].clone(),
            })
            .collect();
        return run_socks_frontend_once(&args[2], endpoints);
    }
    if args.len() == 7
        && args[1] == "--socks-serve"
        && args[3] == "--socks-aggregators"
        && args[5] == "--aggregator-target"
    {
        let socks_addrs = parse_aggregators(&args[4])?;
        let endpoints = socks_addrs
            .into_iter()
            .map(|socks_addr| PathEndpoint::Socks {
                socks_addr,
                target_addr: args[6].clone(),
            })
            .collect();
        return run_socks_frontend_server(&args[2], endpoints);
    }
    if args.len() == 5 && args[1] == "--socks-duplex-listen" && args[3] == "--aggregator" {
        return run_socks_duplex_once(&args[2], vec![PathEndpoint::Direct(args[4].clone())]);
    }
    if args.len() == 7
        && args[1] == "--socks-duplex-listen"
        && args[3] == "--socks-aggregators"
        && args[5] == "--aggregator-target"
    {
        let socks_addrs = parse_aggregators(&args[4])?;
        let endpoints = socks_addrs
            .into_iter()
            .map(|socks_addr| PathEndpoint::Socks {
                socks_addr,
                target_addr: args[6].clone(),
            })
            .collect();
        return run_socks_duplex_once(&args[2], endpoints);
    }
    if args.len() == 7
        && args[1] == "--socks-duplex-serve"
        && args[3] == "--socks-aggregators"
        && args[5] == "--aggregator-target"
    {
        let socks_addrs = parse_aggregators(&args[4])?;
        let endpoints = socks_addrs
            .into_iter()
            .map(|socks_addr| PathEndpoint::Socks {
                socks_addr,
                target_addr: args[6].clone(),
            })
            .collect();
        return run_socks_duplex_server(&args[2], endpoints);
    }
    Err(usage())
}

fn usage() -> String {
    "usage: wbstream_multipath_client --listen 127.0.0.1:PORT (--aggregator 127.0.0.1:PORT|--aggregators 127.0.0.1:PORT,127.0.0.1:PORT|--socks-aggregators 127.0.0.1:PORT,127.0.0.1:PORT --target 127.0.0.1:PORT) OR (--socks-listen|--socks-serve) 127.0.0.1:PORT --socks-aggregators 127.0.0.1:PORT,127.0.0.1:PORT --aggregator-target 127.0.0.1:PORT OR (--socks-duplex-listen|--socks-duplex-serve) 127.0.0.1:PORT (--aggregator 127.0.0.1:PORT|--socks-aggregators 127.0.0.1:PORT,127.0.0.1:PORT --aggregator-target 127.0.0.1:PORT)"
        .to_string()
}

#[derive(Clone, Debug)]
enum PathEndpoint {
    Direct(String),
    Socks {
        socks_addr: String,
        target_addr: String,
    },
}

impl PathEndpoint {
    fn label(&self) -> String {
        match self {
            Self::Direct(addr) => addr.clone(),
            Self::Socks {
                socks_addr,
                target_addr,
            } => format!("socks5://{} -> {}", socks_addr, target_addr),
        }
    }
}

fn parse_aggregators(value: &str) -> Result<Vec<String>, String> {
    let aggregators: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|addr| !addr.is_empty())
        .map(str::to_string)
        .collect();
    if aggregators.is_empty() {
        return Err("at least one aggregator address is required".to_string());
    }
    if aggregators.len() > u8::MAX as usize {
        return Err("at most 255 aggregator paths are supported".to_string());
    }
    Ok(aggregators)
}

fn run_client_once(listen_addr: &str, endpoints: Vec<PathEndpoint>) -> Result<(), String> {
    let listener =
        TcpListener::bind(listen_addr).map_err(|e| format!("listen {}: {}", listen_addr, e))?;
    eprintln!(
        "wbstream_multipath_client listening {} -> {}",
        listener.local_addr().map_err(|e| e.to_string())?,
        endpoints
            .iter()
            .map(PathEndpoint::label)
            .collect::<Vec<_>>()
            .join(",")
    );
    let (mut inbound, _) = listener.accept().map_err(|e| format!("accept: {}", e))?;
    handle_connection(&mut inbound, &endpoints)
}

fn run_socks_frontend_once(listen_addr: &str, endpoints: Vec<PathEndpoint>) -> Result<(), String> {
    let listener =
        TcpListener::bind(listen_addr).map_err(|e| format!("listen {}: {}", listen_addr, e))?;
    eprintln!(
        "wbstream_multipath_client socks listening {} -> {}",
        listener.local_addr().map_err(|e| e.to_string())?,
        endpoints
            .iter()
            .map(PathEndpoint::label)
            .collect::<Vec<_>>()
            .join(",")
    );
    let (mut inbound, _) = listener.accept().map_err(|e| format!("accept: {}", e))?;
    handle_socks_connection(&mut inbound, &endpoints)
}

fn run_socks_frontend_server(
    listen_addr: &str,
    endpoints: Vec<PathEndpoint>,
) -> Result<(), String> {
    let listener =
        TcpListener::bind(listen_addr).map_err(|e| format!("listen {}: {}", listen_addr, e))?;
    eprintln!(
        "wbstream_multipath_client socks serving {} -> {}",
        listener.local_addr().map_err(|e| e.to_string())?,
        endpoints
            .iter()
            .map(PathEndpoint::label)
            .collect::<Vec<_>>()
            .join(",")
    );
    for inbound in listener.incoming() {
        match inbound {
            Ok(mut inbound) => {
                if let Err(e) = handle_socks_connection(&mut inbound, &endpoints) {
                    eprintln!("wbstream_multipath_client socks connection error: {}", e);
                }
            }
            Err(e) => eprintln!("wbstream_multipath_client socks accept error: {}", e),
        }
    }
    Ok(())
}

fn run_socks_duplex_once(listen_addr: &str, endpoints: Vec<PathEndpoint>) -> Result<(), String> {
    let listener =
        TcpListener::bind(listen_addr).map_err(|e| format!("listen {}: {}", listen_addr, e))?;
    eprintln!(
        "wbstream_multipath_client socks duplex listening {} -> {}",
        listener.local_addr().map_err(|e| e.to_string())?,
        endpoints
            .iter()
            .map(PathEndpoint::label)
            .collect::<Vec<_>>()
            .join(",")
    );
    let (mut inbound, _) = listener.accept().map_err(|e| format!("accept: {}", e))?;
    handle_socks_duplex_connection(&mut inbound, &endpoints)
}

fn run_socks_duplex_server(listen_addr: &str, endpoints: Vec<PathEndpoint>) -> Result<(), String> {
    let listener =
        TcpListener::bind(listen_addr).map_err(|e| format!("listen {}: {}", listen_addr, e))?;
    eprintln!(
        "wbstream_multipath_client socks duplex serving {} -> {}",
        listener.local_addr().map_err(|e| e.to_string())?,
        endpoints
            .iter()
            .map(PathEndpoint::label)
            .collect::<Vec<_>>()
            .join(",")
    );
    for inbound in listener.incoming() {
        match inbound {
            Ok(mut inbound) => {
                if let Err(e) = handle_socks_duplex_connection(&mut inbound, &endpoints) {
                    eprintln!("wbstream_multipath_client socks duplex error: {}", e);
                }
            }
            Err(e) => eprintln!("wbstream_multipath_client socks duplex accept error: {}", e),
        }
    }
    Ok(())
}

fn handle_connection(inbound: &mut TcpStream, endpoints: &[PathEndpoint]) -> Result<(), String> {
    let mut plain = Vec::new();
    inbound
        .read_to_end(&mut plain)
        .map_err(|e| format!("read inbound: {}", e))?;
    let response = send_plain_over_paths(&plain, endpoints)?;
    inbound
        .write_all(&response)
        .map_err(|e| format!("write inbound: {}", e))?;
    Ok(())
}

fn handle_socks_connection(
    inbound: &mut TcpStream,
    endpoints: &[PathEndpoint],
) -> Result<(), String> {
    let target_addr = accept_socks5_connect(inbound)?;
    let mut payload = Vec::new();
    inbound
        .read_to_end(&mut payload)
        .map_err(|e| format!("read socks payload: {}", e))?;
    let plain = encode_proxy_request(&target_addr, &payload)?;
    let response = send_plain_over_paths(&plain, endpoints)?;
    inbound
        .write_all(&response)
        .map_err(|e| format!("write socks response: {}", e))?;
    Ok(())
}

fn handle_socks_duplex_connection(
    inbound: &mut TcpStream,
    endpoints: &[PathEndpoint],
) -> Result<(), String> {
    let target_addr = accept_socks5_connect(inbound)?;
    let session_id = session_id();
    let mut writers = HashMap::new();
    let mut path_scores = Vec::new();
    let (response_tx, response_rx) = mpsc::channel();
    for (index, endpoint) in endpoints.iter().enumerate() {
        let path_id = index as u8 + 1;
        let path = connect_endpoint_with_retry(endpoint, Duration::from_secs(3))?;
        writers.insert(
            path_id,
            Arc::new(Mutex::new(path.try_clone().map_err(|e| {
                format!("clone duplex path writer {}: {}", path_id, e)
            })?)),
        );
        path_scores.push(PathScore {
            path_id,
            rtt_ms: 10 + index as u32 * 10,
            inflight_bytes: 0,
            healthy: true,
        });
        let tx = response_tx.clone();
        thread::spawn(move || read_frames_until_eof(path, tx));
    }
    drop(response_tx);
    let writers = Arc::new(writers);
    let upload_splitter = Arc::new(Mutex::new(StreamSplitter::new(
        session_id,
        1,
        DEFAULT_CHUNK_SIZE,
    )?));
    let upload_scheduler = Arc::new(Mutex::new(PathScheduler::new(path_scores)?));

    let prelude = encode_proxy_prelude(&target_addr)?;
    write_split_frames(
        &writers,
        &upload_splitter,
        &upload_scheduler,
        &prelude,
        false,
    )?;

    let inbound_reader = inbound
        .try_clone()
        .map_err(|e| format!("clone socks inbound reader: {}", e))?;
    let upload_writers = writers.clone();
    let upload_splitter = upload_splitter.clone();
    let upload_scheduler = upload_scheduler.clone();
    let _upload = thread::spawn(move || -> Result<(), String> {
        let mut reader = inbound_reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    write_split_frames(
                        &upload_writers,
                        &upload_splitter,
                        &upload_scheduler,
                        b"",
                        true,
                    )?;
                    break;
                }
                Ok(n) => write_split_frames(
                    &upload_writers,
                    &upload_splitter,
                    &upload_scheduler,
                    &buf[..n],
                    false,
                )?,
                Err(e) => return Err(format!("read socks duplex inbound: {}", e)),
            }
        }
        Ok(())
    });

    let mut reassembler = StreamReassembler::new(1024 * 1024);
    for frame in response_rx {
        if frame.stream_id != 2 {
            continue;
        }
        let output = reassembler.push(frame)?;
        if !output.bytes.is_empty() {
            inbound
                .write_all(&output.bytes)
                .map_err(|e| format!("write socks duplex response: {}", e))?;
        }
        if output.complete {
            break;
        }
    }
    Ok(())
}

fn send_plain_over_paths(plain: &[u8], endpoints: &[PathEndpoint]) -> Result<Vec<u8>, String> {
    let path_records = split_to_path_records(&plain, endpoints.len())?;

    let mut workers = Vec::new();
    for (index, endpoint) in endpoints.iter().enumerate() {
        let endpoint = endpoint.clone();
        let records = path_records
            .get(&(index as u8 + 1))
            .cloned()
            .unwrap_or_default();
        eprintln!(
            "wbstream_multipath_client path={} endpoint={} framed_bytes={}",
            index + 1,
            endpoint.label(),
            records.len()
        );
        workers.push(thread::spawn(move || {
            send_path_records(&endpoint, &records)
        }));
    }

    let mut response = Vec::new();
    for worker in workers {
        let path_response = worker
            .join()
            .map_err(|_| "multipath path worker panicked".to_string())??;
        if response.is_empty() && !path_response.is_empty() {
            response = path_response;
        }
    }
    Ok(response)
}

fn accept_socks5_connect(stream: &mut TcpStream) -> Result<String, String> {
    let mut greeting_header = [0u8; 2];
    stream
        .read_exact(&mut greeting_header)
        .map_err(|e| format!("read socks greeting header: {}", e))?;
    if greeting_header[0] != 0x05 || greeting_header[1] == 0 {
        return Err(format!(
            "unsupported socks greeting: {:02x?}",
            greeting_header
        ));
    }
    let mut methods = vec![0u8; greeting_header[1] as usize];
    stream
        .read_exact(&mut methods)
        .map_err(|e| format!("read socks methods: {}", e))?;
    if !methods.contains(&0x00) {
        stream
            .write_all(&[0x05, 0xff])
            .map_err(|e| format!("write socks auth rejection: {}", e))?;
        return Err("socks client did not offer no-auth".to_string());
    }
    stream
        .write_all(&[0x05, 0x00])
        .map_err(|e| format!("write socks greeting response: {}", e))?;

    let mut request_header = [0u8; 4];
    stream
        .read_exact(&mut request_header)
        .map_err(|e| format!("read socks request header: {}", e))?;
    if request_header[..3] != [0x05, 0x01, 0x00] {
        return Err(format!(
            "unsupported socks request: {:02x?}",
            request_header
        ));
    }
    let host = match request_header[3] {
        0x01 => {
            let mut ip = [0u8; 4];
            stream
                .read_exact(&mut ip)
                .map_err(|e| format!("read socks ipv4 target: {}", e))?;
            format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream
                .read_exact(&mut len)
                .map_err(|e| format!("read socks domain length: {}", e))?;
            let mut host = vec![0u8; len[0] as usize];
            stream
                .read_exact(&mut host)
                .map_err(|e| format!("read socks domain: {}", e))?;
            String::from_utf8(host).map_err(|e| format!("socks domain is not utf8: {}", e))?
        }
        atyp => return Err(format!("unsupported socks target atyp {}", atyp)),
    };
    let mut port = [0u8; 2];
    stream
        .read_exact(&mut port)
        .map_err(|e| format!("read socks target port: {}", e))?;
    stream
        .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
        .map_err(|e| format!("write socks connect response: {}", e))?;
    Ok(format!("{}:{}", host, u16::from_be_bytes(port)))
}

fn encode_proxy_request(target_addr: &str, payload: &[u8]) -> Result<Vec<u8>, String> {
    if target_addr.as_bytes().contains(&b'\n') {
        return Err("proxy target must not contain newline".to_string());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"WBSMPROXY/1\nTARGET ");
    out.extend_from_slice(target_addr.as_bytes());
    out.extend_from_slice(b"\n\n");
    out.extend_from_slice(payload);
    Ok(out)
}

fn encode_proxy_prelude(target_addr: &str) -> Result<Vec<u8>, String> {
    encode_proxy_request(target_addr, &[])
}

fn write_split_frames(
    writers: &Arc<HashMap<u8, Arc<Mutex<TcpStream>>>>,
    splitter: &Arc<Mutex<StreamSplitter>>,
    scheduler: &Arc<Mutex<PathScheduler>>,
    bytes: &[u8],
    finish: bool,
) -> Result<(), String> {
    let mut splitter = splitter
        .lock()
        .map_err(|_| "duplex splitter lock poisoned".to_string())?;
    let mut scheduler = scheduler
        .lock()
        .map_err(|_| "duplex scheduler lock poisoned".to_string())?;
    let frames = splitter.split(bytes, finish, &mut scheduler)?;
    for frame in frames {
        let writer = writers
            .get(&frame.path_id)
            .ok_or_else(|| format!("missing duplex writer for path {}", frame.path_id))?;
        let mut writer = writer
            .lock()
            .map_err(|_| "duplex writer lock poisoned".to_string())?;
        writer
            .write_all(&encode_transport_record(&frame)?)
            .map_err(|e| format!("write duplex frame: {}", e))?;
        writer
            .flush()
            .map_err(|e| format!("flush duplex frame: {}", e))?;
    }
    Ok(())
}

fn read_frames_until_eof(
    mut stream: TcpStream,
    tx: mpsc::Sender<wbstream_multipath::MultipathFrame>,
) {
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
                Err(_) => return,
            },
            Err(_) => break,
        }
    }
}

fn send_path_records(endpoint: &PathEndpoint, records: &[u8]) -> Result<Vec<u8>, String> {
    let mut aggregator = connect_endpoint_with_retry(endpoint, Duration::from_secs(3))?;
    aggregator
        .write_all(records)
        .map_err(|e| format!("write aggregator {}: {}", endpoint.label(), e))?;
    aggregator
        .shutdown(Shutdown::Write)
        .map_err(|e| format!("shutdown aggregator {} write: {}", endpoint.label(), e))?;
    let mut response = Vec::new();
    aggregator
        .read_to_end(&mut response)
        .map_err(|e| format!("read aggregator {}: {}", endpoint.label(), e))?;
    Ok(response)
}

fn connect_endpoint_with_retry(
    endpoint: &PathEndpoint,
    timeout: Duration,
) -> Result<TcpStream, String> {
    let started = Instant::now();
    loop {
        let attempt = match endpoint {
            PathEndpoint::Direct(addr) => {
                TcpStream::connect(addr).map_err(|e| format!("connect aggregator {}: {}", addr, e))
            }
            PathEndpoint::Socks {
                socks_addr,
                target_addr,
            } => connect_via_socks5(socks_addr, target_addr),
        };
        match attempt {
            Ok(stream) => return Ok(stream),
            Err(e) if started.elapsed() < timeout => {
                let _ = e;
                sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
}

fn connect_via_socks5(socks_addr: &str, target_addr: &str) -> Result<TcpStream, String> {
    let mut stream = TcpStream::connect(socks_addr)
        .map_err(|e| format!("connect socks {}: {}", socks_addr, e))?;
    stream
        .write_all(&[0x05, 0x01, 0x00])
        .map_err(|e| format!("write socks greeting {}: {}", socks_addr, e))?;
    let mut greeting_response = [0u8; 2];
    stream
        .read_exact(&mut greeting_response)
        .map_err(|e| format!("read socks greeting {}: {}", socks_addr, e))?;
    if greeting_response != [0x05, 0x00] {
        return Err(format!(
            "socks {} rejected no-auth greeting: {:02x?}",
            socks_addr, greeting_response
        ));
    }

    let (host, port) = split_host_port(target_addr)?;
    let mut request = vec![0x05, 0x01, 0x00];
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        request.push(0x01);
        request.extend_from_slice(&ip.octets());
    } else {
        if host.len() > u8::MAX as usize {
            return Err("socks target host is too long".to_string());
        }
        request.push(0x03);
        request.push(host.len() as u8);
        request.extend_from_slice(host.as_bytes());
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream
        .write_all(&request)
        .map_err(|e| format!("write socks connect {}: {}", socks_addr, e))?;

    let mut reply = [0u8; 4];
    stream
        .read_exact(&mut reply)
        .map_err(|e| format!("read socks connect {}: {}", socks_addr, e))?;
    if reply[0] != 0x05 || reply[1] != 0x00 {
        return Err(format!(
            "socks {} connect to {} failed: {:02x?}",
            socks_addr, target_addr, reply
        ));
    }
    match reply[3] {
        0x01 => {
            let mut rest = [0u8; 6];
            stream
                .read_exact(&mut rest)
                .map_err(|e| format!("read socks ipv4 bind {}: {}", socks_addr, e))?;
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream
                .read_exact(&mut len)
                .map_err(|e| format!("read socks domain bind length {}: {}", socks_addr, e))?;
            let mut rest = vec![0u8; len[0] as usize + 2];
            stream
                .read_exact(&mut rest)
                .map_err(|e| format!("read socks domain bind {}: {}", socks_addr, e))?;
        }
        atyp => return Err(format!("unsupported socks bind atyp {}", atyp)),
    }
    Ok(stream)
}

fn split_host_port(addr: &str) -> Result<(&str, u16), String> {
    let (host, port) = addr
        .rsplit_once(':')
        .ok_or_else(|| format!("target address missing port: {}", addr))?;
    if host.is_empty() {
        return Err(format!("target address missing host: {}", addr));
    }
    let port = port
        .parse::<u16>()
        .map_err(|e| format!("parse target port {}: {}", addr, e))?;
    Ok((host, port))
}

fn split_to_path_records(plain: &[u8], path_count: usize) -> Result<BTreeMap<u8, Vec<u8>>, String> {
    let mut scores = Vec::new();
    for index in 0..path_count {
        scores.push(PathScore {
            path_id: index as u8 + 1,
            rtt_ms: 10 + index as u32 * 10,
            inflight_bytes: 0,
            healthy: true,
        });
    }
    let mut scheduler = PathScheduler::new(scores)?;
    let mut splitter = StreamSplitter::new(session_id(), 1, DEFAULT_CHUNK_SIZE)?;
    let frames = splitter.split(plain, true, &mut scheduler)?;
    let mut out = BTreeMap::new();
    for frame in frames {
        out.entry(frame.path_id)
            .or_insert_with(Vec::new)
            .extend_from_slice(&encode_transport_record(&frame)?);
    }
    Ok(out)
}

fn session_id() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
}
