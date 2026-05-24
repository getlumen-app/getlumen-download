use app_lib::wbstream_multipath::{encode_transport_record, MultipathFrame, FRAME_FLAG_FIN};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

#[test]
fn aggregator_binary_reassembles_framed_records_from_stdin() {
    let frames = [
        MultipathFrame {
            session_id: 1,
            stream_id: 1,
            seq: 1,
            offset: 6,
            path_id: 2,
            flags: FRAME_FLAG_FIN,
            payload: b"world".to_vec(),
        },
        MultipathFrame {
            session_id: 1,
            stream_id: 1,
            seq: 0,
            offset: 0,
            path_id: 1,
            flags: 0,
            payload: b"hello ".to_vec(),
        },
    ];
    let mut stdin_payload = Vec::new();
    for frame in frames {
        stdin_payload.extend_from_slice(&encode_transport_record(&frame).unwrap());
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&stdin_payload)
        .unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"hello world");
}

#[test]
fn aggregator_binary_rejects_incomplete_stream() {
    let frame = MultipathFrame {
        session_id: 1,
        stream_id: 1,
        seq: 0,
        offset: 0,
        path_id: 1,
        flags: 0,
        payload: b"unfinished".to_vec(),
    };
    let stdin_payload = encode_transport_record(&frame).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&stdin_payload)
        .unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("did not complete"));
}

#[test]
fn aggregator_binary_tcp_mode_reassembles_one_connection() {
    let addr = reserve_local_addr();
    let child = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args(["--listen", &addr])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stream = connect_with_retry(&addr);
    stream
        .write_all(&framed_payload([
            ("hello ", 0, 0),
            ("tcp", 6, FRAME_FLAG_FIN),
        ]))
        .unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    read_to_end_allow_reset(&mut stream, &mut response).unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(response, b"hello tcp");
}

#[test]
fn aggregator_binary_persistent_server_supports_health_and_data_connections() {
    let addr = reserve_local_addr();
    let mut child = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args(["--serve", &addr])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut health = connect_with_retry(&addr);
    health
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    health
        .write_all(b"GET /health HTTP/1.1\r\nhost: local\r\n\r\n")
        .unwrap();
    let mut health_response_bytes = Vec::new();
    read_to_end_allow_reset(&mut health, &mut health_response_bytes).unwrap();
    let health_response = String::from_utf8(health_response_bytes).unwrap();
    assert!(health_response.contains("HTTP/1.1 200 OK"));
    assert!(health_response.contains("\"ok\":true"));
    assert!(health_response.contains("wbstream_multipath_aggregator"));

    let mut data = connect_with_retry(&addr);
    data.write_all(&framed_payload([
        ("persist ", 0, 0),
        ("ok", 8, FRAME_FLAG_FIN),
    ]))
    .unwrap();
    data.shutdown(Shutdown::Write).unwrap();
    let mut data_response = Vec::new();
    read_to_end_allow_reset(&mut data, &mut data_response).unwrap();
    assert_eq!(data_response, b"persist ok");

    child.kill().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
}

#[test]
fn aggregator_binary_multipath_mode_completes_on_fin_without_waiting_for_path_eof() {
    let addr = reserve_local_addr();
    let child = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args(["--serve-multipath", &addr, "--paths", "3"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut path1 = connect_with_retry(&addr);
    let mut path2 = connect_with_retry(&addr);
    let mut path3 = connect_with_retry(&addr);

    path1
        .write_all(&framed_payload_for_path([("hello ", 0, 0, 1)]))
        .unwrap();
    path2
        .write_all(&framed_payload_for_path([("fin ", 6, 0, 2)]))
        .unwrap();
    path3
        .write_all(&framed_payload_for_path([("early", 10, FRAME_FLAG_FIN, 3)]))
        .unwrap();

    path1
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut response = Vec::new();
    path1.read_to_end(&mut response).unwrap();
    assert_eq!(response, b"hello fin early");

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn framed_payload<const N: usize>(chunks: [(&str, u64, u8); N]) -> Vec<u8> {
    let mut payload = Vec::new();
    for (seq, (chunk, offset, flags)) in chunks.into_iter().enumerate() {
        let frame = MultipathFrame {
            session_id: 2,
            stream_id: 1,
            seq: seq as u64,
            offset,
            path_id: 1,
            flags,
            payload: chunk.as_bytes().to_vec(),
        };
        payload.extend_from_slice(&encode_transport_record(&frame).unwrap());
    }
    payload
}

fn framed_payload_for_path<const N: usize>(chunks: [(&str, u64, u8, u8); N]) -> Vec<u8> {
    let mut payload = Vec::new();
    for (seq, (chunk, offset, flags, path_id)) in chunks.into_iter().enumerate() {
        let frame = MultipathFrame {
            session_id: 3,
            stream_id: 1,
            seq: seq as u64,
            offset,
            path_id,
            flags,
            payload: chunk.as_bytes().to_vec(),
        };
        payload.extend_from_slice(&encode_transport_record(&frame).unwrap());
    }
    payload
}

fn reserve_local_addr() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    addr
}

fn connect_with_retry(addr: &str) -> TcpStream {
    let started = Instant::now();
    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => return stream,
            Err(e) if started.elapsed() < Duration::from_secs(3) => {
                let _ = e;
                sleep(Duration::from_millis(25));
            }
            Err(e) => panic!("connect {}: {}", addr, e),
        }
    }
}

fn read_to_end_allow_reset(stream: &mut TcpStream, response: &mut Vec<u8>) -> std::io::Result<()> {
    match stream.read_to_end(response) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == ErrorKind::ConnectionReset && !response.is_empty() => Ok(()),
        Err(e) => Err(e),
    }
}
