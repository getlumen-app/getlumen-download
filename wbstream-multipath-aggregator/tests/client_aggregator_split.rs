use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, sleep, JoinHandle};
use std::time::{Duration, Instant};

static PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn client_binary_sends_plain_tcp_stream_through_aggregator_transport() {
    let _guard = process_test_guard();
    let aggregator_addr = reserve_local_addr();
    let mut aggregator = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args(["--serve", &aggregator_addr])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_tcp(&aggregator_addr);

    let client_addr = reserve_local_addr();
    let client = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_client"))
        .args(["--listen", &client_addr, "--aggregator", &aggregator_addr])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut plain = connect_with_retry(&client_addr);
    plain
        .write_all(b"plain tcp over framed multipath transport")
        .unwrap();
    plain.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    plain.read_to_end(&mut response).unwrap();

    let client_output = client.wait_with_output().unwrap();
    assert!(
        client_output.status.success(),
        "client stderr={}",
        String::from_utf8_lossy(&client_output.stderr)
    );
    kill_and_wait(&mut aggregator);
    assert_eq!(response, b"plain tcp over framed multipath transport");
}

#[test]
fn client_binary_can_use_mock_wb_path_process_between_client_and_aggregator() {
    let _guard = process_test_guard();
    let aggregator_addr = reserve_local_addr();
    let mut aggregator = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args(["--serve", &aggregator_addr])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_tcp(&aggregator_addr);

    let path_addr = reserve_local_addr();
    let mock_path = Command::new(env!("CARGO_BIN_EXE_wbstream_mock_path"))
        .args([
            "--listen",
            &path_addr,
            "--upstream",
            &aggregator_addr,
            "--delay-ms",
            "5",
            "--fragment-size",
            "7",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let client_addr = reserve_local_addr();
    let client = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_client"))
        .args(["--listen", &client_addr, "--aggregator", &path_addr])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut plain = connect_with_retry(&client_addr);
    plain
        .write_all(b"plain tcp through controlled mock wb path")
        .unwrap();
    plain.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    plain.read_to_end(&mut response).unwrap();

    let client_output = client.wait_with_output().unwrap();
    assert!(
        client_output.status.success(),
        "client stderr={}",
        String::from_utf8_lossy(&client_output.stderr)
    );
    let path_output = mock_path.wait_with_output().unwrap();
    assert!(
        path_output.status.success(),
        "path stderr={}",
        String::from_utf8_lossy(&path_output.stderr)
    );
    kill_and_wait(&mut aggregator);
    assert_eq!(response, b"plain tcp through controlled mock wb path");
}

#[test]
fn client_binary_splits_one_plain_stream_across_three_mock_wb_paths() {
    let _guard = process_test_guard();
    let aggregator_addr = reserve_local_addr();
    let mut aggregator = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args(["--serve-multipath", &aggregator_addr, "--paths", "3"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let path_addrs = [
        reserve_local_addr(),
        reserve_local_addr(),
        reserve_local_addr(),
    ];
    let mut mock_paths = Vec::new();
    for (path_addr, delay_ms, fragment_size) in [
        (&path_addrs[0], "9", "5"),
        (&path_addrs[1], "2", "13"),
        (&path_addrs[2], "15", "3"),
    ] {
        let mock_path = Command::new(env!("CARGO_BIN_EXE_wbstream_mock_path"))
            .args([
                "--listen",
                path_addr,
                "--upstream",
                &aggregator_addr,
                "--delay-ms",
                delay_ms,
                "--fragment-size",
                fragment_size,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        mock_paths.push(mock_path);
    }

    let client_addr = reserve_local_addr();
    let client = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_client"))
        .args([
            "--listen",
            &client_addr,
            "--aggregators",
            &path_addrs.join(","),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let payload = b"this single stream is intentionally long enough to be split across three independent mock wb paths before reassembly";
    let mut plain = connect_with_retry(&client_addr);
    plain.write_all(payload).unwrap();
    plain.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    plain.read_to_end(&mut response).unwrap();

    let client_output = client.wait_with_output().unwrap();
    assert!(
        client_output.status.success(),
        "client stderr={}",
        String::from_utf8_lossy(&client_output.stderr)
    );
    for mock_path in mock_paths {
        let path_output = mock_path.wait_with_output().unwrap();
        assert!(
            path_output.status.success(),
            "path stderr={}",
            String::from_utf8_lossy(&path_output.stderr)
        );
    }
    kill_and_wait(&mut aggregator);
    assert_eq!(response, payload);
}

#[test]
fn client_binary_splits_one_plain_stream_across_three_socks_paths() {
    let _guard = process_test_guard();
    let aggregator_addr = reserve_local_addr();
    let mut aggregator = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args(["--serve-multipath", &aggregator_addr, "--paths", "3"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let socks_addrs = [
        reserve_local_addr(),
        reserve_local_addr(),
        reserve_local_addr(),
    ];
    let socks_workers: Vec<JoinHandle<()>> = socks_addrs
        .iter()
        .map(|addr| start_one_fake_socks_path(addr.clone()))
        .collect();

    let client_addr = reserve_local_addr();
    let client = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_client"))
        .args([
            "--listen",
            &client_addr,
            "--socks-aggregators",
            &socks_addrs.join(","),
            "--target",
            &aggregator_addr,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let payload =
        b"single stream split through three socks legs that stand in for wb joiner transports";
    let mut plain = connect_with_retry(&client_addr);
    plain.write_all(payload).unwrap();
    plain.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    plain.read_to_end(&mut response).unwrap();

    let client_output = client.wait_with_output().unwrap();
    assert!(
        client_output.status.success(),
        "client stderr={}",
        String::from_utf8_lossy(&client_output.stderr)
    );
    for worker in socks_workers {
        worker.join().unwrap();
    }
    kill_and_wait(&mut aggregator);
    assert_eq!(response, payload);
}

#[test]
fn socks_frontend_splits_connect_stream_across_three_socks_paths_to_target() {
    let _guard = process_test_guard();
    let target_addr = reserve_local_addr();
    let target = start_one_echo_http_target(target_addr.clone());

    let aggregator_addr = reserve_local_addr();
    let mut aggregator = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args(["--serve-proxy-multipath", &aggregator_addr, "--paths", "3"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let socks_addrs = [
        reserve_local_addr(),
        reserve_local_addr(),
        reserve_local_addr(),
    ];
    let socks_workers: Vec<JoinHandle<()>> = socks_addrs
        .iter()
        .map(|addr| start_one_fake_socks_path(addr.clone()))
        .collect();

    let client_addr = reserve_local_addr();
    let client = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_client"))
        .args([
            "--socks-listen",
            &client_addr,
            "--socks-aggregators",
            &socks_addrs.join(","),
            "--aggregator-target",
            &aggregator_addr,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut browser_side = connect_with_retry(&client_addr);
    socks_connect(&mut browser_side, &target_addr);
    browser_side
        .write_all(b"GET /through-multipath HTTP/1.1\r\nhost: example.test\r\n\r\n")
        .unwrap();
    browser_side.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    browser_side.read_to_end(&mut response).unwrap();

    let client_output = client.wait_with_output().unwrap();
    assert!(
        client_output.status.success(),
        "client stderr={}",
        String::from_utf8_lossy(&client_output.stderr)
    );
    for worker in socks_workers {
        worker.join().unwrap();
    }
    target.join().unwrap();
    kill_and_wait(&mut aggregator);
    assert!(String::from_utf8_lossy(&response).contains("through multipath target"));
}

#[test]
fn persistent_socks_frontend_handles_two_sequential_proxy_connects() {
    let _guard = process_test_guard();
    let target_addr = reserve_local_addr();
    let target = start_echo_http_target(target_addr.clone(), 2);

    let aggregator_addr = reserve_local_addr();
    let mut aggregator = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args([
            "--serve-proxy-multipath-loop",
            &aggregator_addr,
            "--paths",
            "3",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let socks_addrs = [
        reserve_local_addr(),
        reserve_local_addr(),
        reserve_local_addr(),
    ];
    let socks_workers: Vec<JoinHandle<()>> = socks_addrs
        .iter()
        .map(|addr| start_fake_socks_path(addr.clone(), 2))
        .collect();

    let client_addr = reserve_local_addr();
    let mut client = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_client"))
        .args([
            "--socks-serve",
            &client_addr,
            "--socks-aggregators",
            &socks_addrs.join(","),
            "--aggregator-target",
            &aggregator_addr,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let first = socks_http_once(&client_addr, &target_addr);
    let second = socks_http_once(&client_addr, &target_addr);

    kill_and_wait(&mut client);
    for worker in socks_workers {
        worker.join().unwrap();
    }
    target.join().unwrap();
    kill_and_wait(&mut aggregator);
    assert!(String::from_utf8_lossy(&first).contains("through multipath target"));
    assert!(String::from_utf8_lossy(&second).contains("through multipath target"));
}

#[test]
fn duplex_socks_frontend_returns_response_before_client_half_close() {
    let _guard = process_test_guard();
    let target_addr = reserve_local_addr();
    let target = start_early_response_http_target(target_addr.clone());

    let aggregator_addr = reserve_local_addr();
    let aggregator = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args(["--serve-proxy-duplex", &aggregator_addr, "--paths", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let client_addr = reserve_local_addr();
    let client = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_client"))
        .args([
            "--socks-duplex-listen",
            &client_addr,
            "--aggregator",
            &aggregator_addr,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut browser_side = connect_with_retry(&client_addr);
    socks_connect(&mut browser_side, &target_addr);
    browser_side
        .write_all(b"GET /duplex HTTP/1.1\r\nhost: example.test\r\n")
        .unwrap();
    browser_side.write_all(b"\r\n").unwrap();

    let mut response = [0u8; 128];
    browser_side
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let n = browser_side.read(&mut response).unwrap();

    let client_output = client.wait_with_output().unwrap();
    assert!(
        client_output.status.success(),
        "client stderr={}",
        String::from_utf8_lossy(&client_output.stderr)
    );
    let aggregator_output = aggregator.wait_with_output().unwrap();
    assert!(
        aggregator_output.status.success(),
        "aggregator stderr={}",
        String::from_utf8_lossy(&aggregator_output.stderr)
    );
    target.join().unwrap();
    assert!(String::from_utf8_lossy(&response[..n]).contains("duplex response before eof"));
}

#[test]
fn duplex_socks_frontend_splits_upload_across_three_socks_paths_before_half_close() {
    let _guard = process_test_guard();
    let target_addr = reserve_local_addr();
    let target = start_early_response_http_target(target_addr.clone());

    let aggregator_addr = reserve_local_addr();
    let aggregator = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args(["--serve-proxy-duplex", &aggregator_addr, "--paths", "3"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let socks_addrs = [
        reserve_local_addr(),
        reserve_local_addr(),
        reserve_local_addr(),
    ];
    let socks_workers: Vec<JoinHandle<()>> = socks_addrs
        .iter()
        .map(|addr| start_fake_socks_duplex_path(addr.clone()))
        .collect();

    let client_addr = reserve_local_addr();
    let client = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_client"))
        .args([
            "--socks-duplex-listen",
            &client_addr,
            "--socks-aggregators",
            &socks_addrs.join(","),
            "--aggregator-target",
            &aggregator_addr,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut browser_side = connect_with_retry(&client_addr);
    socks_connect(&mut browser_side, &target_addr);
    browser_side
        .write_all(b"GET /duplex HTTP/1.1\r\nhost: example.test\r\n")
        .unwrap();
    browser_side.write_all(b"\r\n").unwrap();

    let mut response = [0u8; 128];
    browser_side
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let n = browser_side.read(&mut response).unwrap();

    let client_output = client.wait_with_output().unwrap();
    assert!(
        client_output.status.success(),
        "client stderr={}",
        String::from_utf8_lossy(&client_output.stderr)
    );
    let aggregator_output = aggregator.wait_with_output().unwrap();
    assert!(
        aggregator_output.status.success(),
        "aggregator stderr={}",
        String::from_utf8_lossy(&aggregator_output.stderr)
    );
    for worker in socks_workers {
        worker.join().unwrap();
    }
    target.join().unwrap();
    assert!(String::from_utf8_lossy(&response[..n]).contains("duplex response before eof"));
}

#[test]
fn persistent_duplex_socks_frontend_handles_two_sequential_three_path_connects() {
    let _guard = process_test_guard();
    let target_addr = reserve_local_addr();
    let target = start_early_response_http_target_n(target_addr.clone(), 2);

    let aggregator_addr = reserve_local_addr();
    let mut aggregator = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args([
            "--serve-proxy-duplex-loop",
            &aggregator_addr,
            "--paths",
            "3",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let socks_addrs = [
        reserve_local_addr(),
        reserve_local_addr(),
        reserve_local_addr(),
    ];
    let socks_workers: Vec<JoinHandle<()>> = socks_addrs
        .iter()
        .map(|addr| start_fake_socks_duplex_path_n(addr.clone(), 2))
        .collect();

    let client_addr = reserve_local_addr();
    let mut client = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_client"))
        .args([
            "--socks-duplex-serve",
            &client_addr,
            "--socks-aggregators",
            &socks_addrs.join(","),
            "--aggregator-target",
            &aggregator_addr,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let first = socks_duplex_http_once(&client_addr, &target_addr);
    let second = socks_duplex_http_once(&client_addr, &target_addr);

    kill_and_wait(&mut client);
    kill_and_wait(&mut aggregator);
    for worker in socks_workers {
        worker.join().unwrap();
    }
    target.join().unwrap();
    assert!(String::from_utf8_lossy(&first).contains("duplex response before eof"));
    assert!(String::from_utf8_lossy(&second).contains("duplex response before eof"));
}

#[test]
fn duplex_socks_frontend_splits_download_across_three_socks_paths() {
    let _guard = process_test_guard();
    let target_addr = reserve_local_addr();
    let target = start_large_response_http_target(target_addr.clone());

    let aggregator_addr = reserve_local_addr();
    let aggregator = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_aggregator"))
        .args([
            "--serve-proxy-duplex",
            &aggregator_addr,
            "--paths",
            "3",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let socks_addrs = [
        reserve_local_addr(),
        reserve_local_addr(),
        reserve_local_addr(),
    ];
    let download_counts = Arc::new(Mutex::new(vec![0usize; socks_addrs.len()]));
    let socks_workers: Vec<JoinHandle<()>> = socks_addrs
        .iter()
        .enumerate()
        .map(|(index, addr)| {
            start_fake_socks_duplex_path_counting_download(
                addr.clone(),
                1,
                index,
                download_counts.clone(),
            )
        })
        .collect();

    let client_addr = reserve_local_addr();
    let client = Command::new(env!("CARGO_BIN_EXE_wbstream_multipath_client"))
        .args([
            "--socks-duplex-listen",
            &client_addr,
            "--socks-aggregators",
            &socks_addrs.join(","),
            "--aggregator-target",
            &aggregator_addr,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let response = socks_large_http_once(&client_addr, &target_addr);

    let client_output = client.wait_with_output().unwrap();
    assert!(
        client_output.status.success(),
        "client stderr={}",
        String::from_utf8_lossy(&client_output.stderr)
    );
    let aggregator_output = aggregator.wait_with_output().unwrap();
    assert!(
        aggregator_output.status.success(),
        "aggregator stderr={}",
        String::from_utf8_lossy(&aggregator_output.stderr)
    );
    for worker in socks_workers {
        worker.join().unwrap();
    }
    target.join().unwrap();

    let response_text = String::from_utf8_lossy(&response);
    assert!(response_text.contains("content-length: 8192"));
    let counts = download_counts.lock().unwrap().clone();
    assert!(
        counts.iter().all(|count| *count > 0),
        "expected response frames on all three paths, got {:?}",
        counts
    );
}

fn start_one_fake_socks_path(addr: String) -> JoinHandle<()> {
    start_fake_socks_path(addr, 1)
}

fn start_fake_socks_path(addr: String, connection_count: usize) -> JoinHandle<()> {
    thread::spawn(move || {
        let listener = TcpListener::bind(&addr).unwrap();
        for _ in 0..connection_count {
            handle_one_fake_socks_connection(&listener);
        }
    })
}

fn handle_one_fake_socks_connection(listener: &TcpListener) {
    let (mut inbound, _) = listener.accept().unwrap();

    let mut greeting = [0u8; 3];
    inbound.read_exact(&mut greeting).unwrap();
    assert_eq!(greeting, [0x05, 0x01, 0x00]);
    inbound.write_all(&[0x05, 0x00]).unwrap();

    let mut header = [0u8; 4];
    inbound.read_exact(&mut header).unwrap();
    assert_eq!(&header[..3], &[0x05, 0x01, 0x00]);
    let target_addr = match header[3] {
        0x01 => {
            let mut ip = [0u8; 4];
            inbound.read_exact(&mut ip).unwrap();
            let mut port = [0u8; 2];
            inbound.read_exact(&mut port).unwrap();
            format!(
                "{}.{}.{}.{}:{}",
                ip[0],
                ip[1],
                ip[2],
                ip[3],
                u16::from_be_bytes(port)
            )
        }
        0x03 => {
            let mut len = [0u8; 1];
            inbound.read_exact(&mut len).unwrap();
            let mut host = vec![0u8; len[0] as usize];
            inbound.read_exact(&mut host).unwrap();
            let mut port = [0u8; 2];
            inbound.read_exact(&mut port).unwrap();
            format!(
                "{}:{}",
                String::from_utf8(host).unwrap(),
                u16::from_be_bytes(port)
            )
        }
        atyp => panic!("unsupported atyp {}", atyp),
    };
    inbound
        .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
        .unwrap();

    let mut payload = Vec::new();
    inbound.read_to_end(&mut payload).unwrap();
    let mut upstream = connect_with_retry(&target_addr);
    upstream.write_all(&payload).unwrap();
    upstream.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    upstream.read_to_end(&mut response).unwrap();
    inbound.write_all(&response).unwrap();
}

fn start_fake_socks_duplex_path(addr: String) -> JoinHandle<()> {
    start_fake_socks_duplex_path_n(addr, 1)
}

fn start_fake_socks_duplex_path_n(addr: String, connection_count: usize) -> JoinHandle<()> {
    thread::spawn(move || {
        let listener = TcpListener::bind(&addr).unwrap();
        listener
            .set_nonblocking(true)
            .expect("set fake socks listener nonblocking");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut handled = 0;
        while handled < connection_count && Instant::now() < deadline {
            if handle_one_fake_socks_duplex_connection(&listener) {
                handled += 1;
            } else {
                sleep(Duration::from_millis(10));
            }
        }
    })
}

fn start_fake_socks_duplex_path_counting_download(
    addr: String,
    connection_count: usize,
    path_index: usize,
    download_counts: Arc<Mutex<Vec<usize>>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let listener = TcpListener::bind(&addr).unwrap();
        listener
            .set_nonblocking(true)
            .expect("set fake socks listener nonblocking");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut handled = 0;
        while handled < connection_count && Instant::now() < deadline {
            if handle_one_fake_socks_duplex_connection_counting_download(
                &listener,
                path_index,
                download_counts.clone(),
            ) {
                handled += 1;
            } else {
                sleep(Duration::from_millis(10));
            }
        }
    })
}

fn handle_one_fake_socks_duplex_connection(listener: &TcpListener) -> bool {
    let (mut inbound, _) = match listener.accept() {
        Ok(accepted) => accepted,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return false,
        Err(e) => panic!("fake socks accept: {}", e),
    };
    inbound
        .set_nonblocking(false)
        .expect("set fake socks stream blocking");
    inbound
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("set fake socks read timeout");
    let Some(target_addr) = accept_fake_socks_connect(&mut inbound) else {
        return false;
    };
    let mut upstream = connect_with_retry(&target_addr);
    let mut upstream_reader = upstream.try_clone().unwrap();
    let mut inbound_writer = inbound.try_clone().unwrap();
    let response_worker = thread::spawn(move || {
        let _ = std::io::copy(&mut upstream_reader, &mut inbound_writer);
    });
    let _ = std::io::copy(&mut inbound, &mut upstream);
    let _ = upstream.shutdown(Shutdown::Write);
    response_worker.join().unwrap();
    true
}

fn handle_one_fake_socks_duplex_connection_counting_download(
    listener: &TcpListener,
    path_index: usize,
    download_counts: Arc<Mutex<Vec<usize>>>,
) -> bool {
    let (mut inbound, _) = match listener.accept() {
        Ok(accepted) => accepted,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return false,
        Err(e) => panic!("fake socks accept: {}", e),
    };
    inbound
        .set_nonblocking(false)
        .expect("set fake socks stream blocking");
    inbound
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("set fake socks read timeout");
    let Some(target_addr) = accept_fake_socks_connect(&mut inbound) else {
        return false;
    };
    let mut upstream = connect_with_retry(&target_addr);
    let mut upstream_reader = upstream.try_clone().unwrap();
    let mut inbound_writer = inbound.try_clone().unwrap();
    let response_worker = thread::spawn(move || {
        let mut buf = [0u8; 512];
        loop {
            match upstream_reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    download_counts.lock().unwrap()[path_index] += n;
                    if inbound_writer.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let _ = std::io::copy(&mut inbound, &mut upstream);
    let _ = upstream.shutdown(Shutdown::Write);
    response_worker.join().unwrap();
    true
}

fn accept_fake_socks_connect(inbound: &mut TcpStream) -> Option<String> {
    let mut greeting = [0u8; 3];
    if let Err(e) = inbound.read_exact(&mut greeting) {
        if e.kind() == std::io::ErrorKind::UnexpectedEof
            || e.kind() == std::io::ErrorKind::WouldBlock
            || e.kind() == std::io::ErrorKind::TimedOut
        {
            return None;
        }
        panic!("read fake socks greeting: {}", e);
    }
    assert_eq!(greeting, [0x05, 0x01, 0x00]);
    inbound.write_all(&[0x05, 0x00]).unwrap();

    let mut header = [0u8; 4];
    if let Err(e) = inbound.read_exact(&mut header) {
        if e.kind() == std::io::ErrorKind::UnexpectedEof
            || e.kind() == std::io::ErrorKind::WouldBlock
            || e.kind() == std::io::ErrorKind::TimedOut
        {
            return None;
        }
        panic!("read fake socks header: {}", e);
    }
    assert_eq!(&header[..3], &[0x05, 0x01, 0x00]);
    let target_addr = match header[3] {
        0x01 => {
            let mut ip = [0u8; 4];
            inbound.read_exact(&mut ip).unwrap();
            let mut port = [0u8; 2];
            inbound.read_exact(&mut port).unwrap();
            format!(
                "{}.{}.{}.{}:{}",
                ip[0],
                ip[1],
                ip[2],
                ip[3],
                u16::from_be_bytes(port)
            )
        }
        0x03 => {
            let mut len = [0u8; 1];
            inbound.read_exact(&mut len).unwrap();
            let mut host = vec![0u8; len[0] as usize];
            inbound.read_exact(&mut host).unwrap();
            let mut port = [0u8; 2];
            inbound.read_exact(&mut port).unwrap();
            format!(
                "{}:{}",
                String::from_utf8(host).unwrap(),
                u16::from_be_bytes(port)
            )
        }
        atyp => panic!("unsupported atyp {}", atyp),
    };
    inbound
        .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
        .unwrap();
    Some(target_addr)
}

fn start_one_echo_http_target(addr: String) -> JoinHandle<()> {
    start_echo_http_target(addr, 1)
}

fn start_echo_http_target(addr: String, connection_count: usize) -> JoinHandle<()> {
    thread::spawn(move || {
        let listener = TcpListener::bind(&addr).unwrap();
        for _ in 0..connection_count {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            stream.read_to_end(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request).contains("GET /through-multipath"));
            let body = b"through multipath target";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                String::from_utf8_lossy(body)
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    })
}

fn start_early_response_http_target(addr: String) -> JoinHandle<()> {
    start_early_response_http_target_n(addr, 1)
}

fn start_early_response_http_target_n(addr: String, connection_count: usize) -> JoinHandle<()> {
    thread::spawn(move || {
        let listener = TcpListener::bind(&addr).unwrap();
        for _ in 0..connection_count {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 64];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                assert_ne!(n, 0, "client closed before HTTP headers completed");
                request.extend_from_slice(&buf[..n]);
            }
            let body = b"duplex response before eof";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                String::from_utf8_lossy(body)
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    })
}

fn start_large_response_http_target(addr: String) -> JoinHandle<()> {
    thread::spawn(move || {
        let listener = TcpListener::bind(&addr).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 64];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let n = stream.read(&mut buf).unwrap();
            assert_ne!(n, 0, "client closed before HTTP headers completed");
            request.extend_from_slice(&buf[..n]);
        }
        let body = vec![b'x'; 8192];
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
    })
}

fn socks_duplex_http_once(client_addr: &str, target_addr: &str) -> Vec<u8> {
    let mut browser_side = connect_with_retry(client_addr);
    socks_connect(&mut browser_side, target_addr);
    browser_side
        .write_all(b"GET /duplex HTTP/1.1\r\nhost: example.test\r\n\r\n")
        .unwrap();
    browser_side
        .set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    let mut out = vec![0u8; 128];
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut used = 0;
    while used < out.len() && Instant::now() < deadline {
        match browser_side.read(&mut out[used..]) {
            Ok(0) => break,
            Ok(n) => {
                used += n;
                if String::from_utf8_lossy(&out[..used]).contains("duplex response before eof") {
                    break;
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                sleep(Duration::from_millis(25));
            }
            Err(e) => panic!("read duplex response: {}", e),
        }
    }
    out.truncate(used);
    out
}

fn socks_large_http_once(client_addr: &str, target_addr: &str) -> Vec<u8> {
    let mut browser_side = connect_with_retry(client_addr);
    socks_connect(&mut browser_side, target_addr);
    browser_side
        .write_all(b"GET /large HTTP/1.1\r\nhost: example.test\r\n\r\n")
        .unwrap();
    browser_side
        .set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    let mut out = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = [0u8; 1024];
    while Instant::now() < deadline {
        match browser_side.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() >= 8192
                    && out
                        .windows(b"\r\n\r\n".len())
                        .any(|window| window == b"\r\n\r\n")
                    && out.ends_with(&vec![b'x'; 8192])
                {
                    break;
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                sleep(Duration::from_millis(25));
            }
            Err(e) => panic!("read large duplex response: {}", e),
        }
    }
    out
}

fn socks_http_once(client_addr: &str, target_addr: &str) -> Vec<u8> {
    let mut browser_side = connect_with_retry(client_addr);
    socks_connect(&mut browser_side, target_addr);
    browser_side
        .write_all(b"GET /through-multipath HTTP/1.1\r\nhost: example.test\r\n\r\n")
        .unwrap();
    browser_side.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    browser_side.read_to_end(&mut response).unwrap();
    response
}

fn socks_connect(stream: &mut TcpStream, target_addr: &str) {
    stream.write_all(&[0x05, 0x01, 0x00]).unwrap();
    let mut greeting = [0u8; 2];
    stream.read_exact(&mut greeting).unwrap();
    assert_eq!(greeting, [0x05, 0x00]);

    let (host, port) = target_addr.rsplit_once(':').unwrap();
    let mut request = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(&port.parse::<u16>().unwrap().to_be_bytes());
    stream.write_all(&request).unwrap();

    let mut reply = [0u8; 10];
    stream.read_exact(&mut reply).unwrap();
    assert_eq!(&reply[..4], &[0x05, 0x00, 0x00, 0x01]);
}

fn reserve_local_addr() -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    addr
}

fn process_test_guard() -> MutexGuard<'static, ()> {
    PROCESS_TEST_LOCK.lock().unwrap()
}

fn wait_for_tcp(addr: &str) {
    let _ = connect_with_retry(addr);
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

fn kill_and_wait(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
