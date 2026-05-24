use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct RoundRobinPorts {
    ports: Vec<u16>,
    next: AtomicUsize,
}

impl RoundRobinPorts {
    pub fn new(ports: Vec<u16>) -> Result<Self, String> {
        if ports.is_empty() {
            return Err("at least one upstream port is required".to_string());
        }
        Ok(Self {
            ports,
            next: AtomicUsize::new(0),
        })
    }

    pub fn next_port(&self) -> u16 {
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.ports.len();
        self.ports[index]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalancerRuntimeStatus {
    pub running: bool,
    pub listen_port: Option<u16>,
    pub upstream_count: usize,
}

#[derive(Debug)]
struct BalancerRuntime {
    listen_port: u16,
    upstream_count: usize,
    handle: JoinHandle<()>,
}

static BALANCER: OnceLock<Mutex<Option<BalancerRuntime>>> = OnceLock::new();

fn balancer_slot() -> &'static Mutex<Option<BalancerRuntime>> {
    BALANCER.get_or_init(|| Mutex::new(None))
}

pub async fn start_balancer(listen_port: u16, upstream_ports: Vec<u16>) -> Result<u16, String> {
    stop_balancer();
    let upstream_count = upstream_ports.len();
    let picker = Arc::new(RoundRobinPorts::new(upstream_ports)?);
    let listener = TcpListener::bind(("127.0.0.1", listen_port))
        .await
        .map_err(|e| format!("WB Stream balancer listen {}: {}", listen_port, e))?;
    let handle = tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else {
                break;
            };
            let picker = picker.clone();
            tokio::spawn(async move {
                if let Err(e) = proxy_socks_flow(client, picker).await {
                    log::warn!("WB Stream balancer flow failed: {}", e);
                }
            });
        }
    });
    *balancer_slot().lock().unwrap() = Some(BalancerRuntime {
        listen_port,
        upstream_count,
        handle,
    });
    Ok(listen_port)
}

pub fn stop_balancer() {
    if let Some(runtime) = balancer_slot().lock().unwrap().take() {
        runtime.handle.abort();
    }
}

pub fn runtime_status() -> BalancerRuntimeStatus {
    let mut slot = balancer_slot().lock().unwrap();
    if let Some(runtime) = slot.as_ref() {
        if runtime.handle.is_finished() {
            *slot = None;
        }
    }
    if let Some(runtime) = slot.as_ref() {
        BalancerRuntimeStatus {
            running: true,
            listen_port: Some(runtime.listen_port),
            upstream_count: runtime.upstream_count,
        }
    } else {
        BalancerRuntimeStatus {
            running: false,
            listen_port: None,
            upstream_count: 0,
        }
    }
}

async fn proxy_socks_flow(
    mut client: TcpStream,
    picker: Arc<RoundRobinPorts>,
) -> Result<(), String> {
    let request = read_client_socks_request(&mut client).await?;
    let upstream_port = picker.next_port();
    let mut upstream = TcpStream::connect(("127.0.0.1", upstream_port))
        .await
        .map_err(|e| format!("connect upstream SOCKS {}: {}", upstream_port, e))?;
    open_upstream_socks(&mut upstream, &request).await?;
    client
        .write_all(&socks_success_reply())
        .await
        .map_err(|e| format!("client socks success write: {}", e))?;
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

async fn read_client_socks_request(client: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut greeting = [0u8; 2];
    client
        .read_exact(&mut greeting)
        .await
        .map_err(|e| format!("read socks greeting: {}", e))?;
    if greeting[0] != 0x05 {
        return Err("only SOCKS5 is supported".to_string());
    }
    let mut methods = vec![0u8; greeting[1] as usize];
    client
        .read_exact(&mut methods)
        .await
        .map_err(|e| format!("read socks methods: {}", e))?;
    client
        .write_all(&[0x05, 0x00])
        .await
        .map_err(|e| format!("write socks auth choice: {}", e))?;

    let mut head = [0u8; 4];
    client
        .read_exact(&mut head)
        .await
        .map_err(|e| format!("read socks request head: {}", e))?;
    if head[0] != 0x05 || head[1] != 0x01 {
        return Err("only SOCKS5 TCP CONNECT is supported".to_string());
    }

    let mut request = head.to_vec();
    match head[3] {
        0x01 => read_exact_append(client, &mut request, 4 + 2).await?,
        0x03 => {
            let mut len = [0u8; 1];
            client
                .read_exact(&mut len)
                .await
                .map_err(|e| format!("read socks domain length: {}", e))?;
            request.push(len[0]);
            read_exact_append(client, &mut request, len[0] as usize + 2).await?;
        }
        0x04 => read_exact_append(client, &mut request, 16 + 2).await?,
        atyp => return Err(format!("unsupported SOCKS address type 0x{:02x}", atyp)),
    }
    Ok(request)
}

async fn read_exact_append(
    stream: &mut TcpStream,
    target: &mut Vec<u8>,
    len: usize,
) -> Result<(), String> {
    let start = target.len();
    target.resize(start + len, 0);
    stream
        .read_exact(&mut target[start..])
        .await
        .map(|_| ())
        .map_err(|e| format!("read socks request body: {}", e))
}

async fn open_upstream_socks(upstream: &mut TcpStream, request: &[u8]) -> Result<(), String> {
    upstream
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .map_err(|e| format!("write upstream greeting: {}", e))?;
    let mut auth = [0u8; 2];
    upstream
        .read_exact(&mut auth)
        .await
        .map_err(|e| format!("read upstream auth: {}", e))?;
    if auth != [0x05, 0x00] {
        return Err(format!("upstream SOCKS auth rejected: {:?}", auth));
    }
    upstream
        .write_all(request)
        .await
        .map_err(|e| format!("write upstream request: {}", e))?;
    read_upstream_socks_reply(upstream).await
}

async fn read_upstream_socks_reply(upstream: &mut TcpStream) -> Result<(), String> {
    let mut head = [0u8; 4];
    upstream
        .read_exact(&mut head)
        .await
        .map_err(|e| format!("read upstream reply head: {}", e))?;
    if head[0] != 0x05 || head[1] != 0x00 {
        return Err(format!("upstream SOCKS connect failed: {:?}", head));
    }
    match head[3] {
        0x01 => skip_exact(upstream, 4 + 2).await,
        0x03 => {
            let mut len = [0u8; 1];
            upstream
                .read_exact(&mut len)
                .await
                .map_err(|e| format!("read upstream reply domain length: {}", e))?;
            skip_exact(upstream, len[0] as usize + 2).await
        }
        0x04 => skip_exact(upstream, 16 + 2).await,
        atyp => Err(format!(
            "unsupported upstream reply address type 0x{:02x}",
            atyp
        )),
    }
}

async fn skip_exact(stream: &mut TcpStream, len: usize) -> Result<(), String> {
    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .await
        .map(|_| ())
        .map_err(|e| format!("read upstream reply body: {}", e))
}

fn socks_success_reply() -> [u8; 10] {
    [0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::runtime::Builder;

    #[test]
    fn round_robin_rejects_empty_upstream_list() {
        assert!(RoundRobinPorts::new(Vec::new()).is_err());
    }

    #[test]
    fn round_robin_cycles_ports_for_new_flows() {
        let picker = RoundRobinPorts::new(vec![11080, 11081, 11082]).unwrap();
        assert_eq!(picker.next_port(), 11080);
        assert_eq!(picker.next_port(), 11081);
        assert_eq!(picker.next_port(), 11082);
        assert_eq!(picker.next_port(), 11080);
    }

    #[test]
    fn balancer_proxies_socks_connect_to_upstream() {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let upstream_port = upstream.local_addr().unwrap().port();
                let upstream_task = tokio::spawn(async move {
                    let (mut stream, _) = upstream.accept().await.unwrap();
                    let mut greeting = [0u8; 3];
                    stream.read_exact(&mut greeting).await.unwrap();
                    assert_eq!(greeting, [0x05, 0x01, 0x00]);
                    stream.write_all(&[0x05, 0x00]).await.unwrap();

                    let mut req = [0u8; 10];
                    stream.read_exact(&mut req).await.unwrap();
                    assert_eq!(&req[..4], &[0x05, 0x01, 0x00, 0x01]);
                    stream
                        .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x12, 0x34])
                        .await
                        .unwrap();

                    let mut payload = [0u8; 4];
                    stream.read_exact(&mut payload).await.unwrap();
                    assert_eq!(&payload, b"ping");
                    stream.write_all(b"pong").await.unwrap();
                });

                let picker = Arc::new(RoundRobinPorts::new(vec![upstream_port]).unwrap());
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let balancer_port = listener.local_addr().unwrap().port();
                let accept_task = tokio::spawn(async move {
                    let (client, _) = listener.accept().await.unwrap();
                    proxy_socks_flow(client, picker).await.unwrap();
                });

                let mut client = TcpStream::connect(("127.0.0.1", balancer_port))
                    .await
                    .unwrap();
                client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
                let mut auth = [0u8; 2];
                client.read_exact(&mut auth).await.unwrap();
                assert_eq!(auth, [0x05, 0x00]);

                client
                    .write_all(&[0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1, 0x01, 0xbb])
                    .await
                    .unwrap();
                let mut reply = [0u8; 10];
                client.read_exact(&mut reply).await.unwrap();
                assert_eq!(reply, socks_success_reply());

                client.write_all(b"ping").await.unwrap();
                let mut pong = [0u8; 4];
                client.read_exact(&mut pong).await.unwrap();
                assert_eq!(&pong, b"pong");

                drop(client);
                accept_task.await.unwrap();
                upstream_task.await.unwrap();
            });
    }
}
