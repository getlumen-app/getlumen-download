/// Lumen Helper Daemon — runs as root via launchd, manages sing-box subprocess for TUN mode.
///
/// Listens on Unix socket `/var/run/io.getlumen.helper.sock` (mode 0660, root:wheel).
/// One JSON request per line, one JSON response per line.
///
/// Build: `cargo build --release --target aarch64-apple-darwin`
/// Install: copied to /Library/PrivilegedHelperTools/io.getlumen.helper by installer.
mod protocol;
mod singbox_mgr;

use protocol::{Request, Response, HELPER_SOCKET_PATH, HELPER_VERSION};
use singbox_mgr::SingboxManager;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("Lumen helper v{} starting", HELPER_VERSION);

    // Remove stale socket if exists
    let _ = std::fs::remove_file(HELPER_SOCKET_PATH);

    // Bind Unix socket
    let listener = UnixListener::bind(HELPER_SOCKET_PATH).map_err(|e| {
        log::error!("Failed to bind {}: {}", HELPER_SOCKET_PATH, e);
        e
    })?;

    // Permissions: rw for owner (root) and group (wheel/admin), nothing for others.
    // The Lumen app process needs to be in the same group; for now use 0666 to allow user app access.
    // TODO: tighten to 0660 + chown root:_lumen_helper after install.rs sets up the group.
    std::fs::set_permissions(HELPER_SOCKET_PATH, std::fs::Permissions::from_mode(0o666))?;

    log::info!("Listening on {}", HELPER_SOCKET_PATH);

    let singbox = Arc::new(SingboxManager::new());

    // Handle SIGTERM/SIGINT for clean shutdown
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    let singbox_for_shutdown = singbox.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = sigterm.recv() => log::info!("SIGTERM received"),
            _ = sigint.recv() => log::info!("SIGINT received"),
        }
        let _ = singbox_for_shutdown.stop().await;
        let _ = std::fs::remove_file(HELPER_SOCKET_PATH);
        log::info!("Helper exiting");
        std::process::exit(0);
    });

    // Accept loop
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let mgr = singbox.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, mgr).await {
                        log::warn!("Client error: {}", e);
                    }
                });
            }
            Err(e) => {
                log::error!("Accept failed: {}", e);
                break;
            }
        }
    }

    Ok(())
}

async fn handle_client(
    stream: UnixStream,
    singbox: Arc<SingboxManager>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(()); // client closed
        }

        let req: Request = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response::Error { message: format!("Invalid JSON: {}", e) };
                write_response(&mut write_half, &resp).await?;
                continue;
            }
        };

        log::info!("Request: {:?}", req);
        let resp = process_request(req, &singbox).await;
        write_response(&mut write_half, &resp).await?;
    }
}

async fn process_request(req: Request, singbox: &SingboxManager) -> Response {
    match req {
        Request::Ping => Response::Pong { version: HELPER_VERSION.to_string() },
        Request::Start { config_path, singbox_path } => {
            // Validate paths exist before spawning
            if !std::path::Path::new(&singbox_path).exists() {
                return Response::Error { message: format!("singbox not found: {}", singbox_path) };
            }
            if !std::path::Path::new(&config_path).exists() {
                return Response::Error { message: format!("config not found: {}", config_path) };
            }
            match singbox.start(&singbox_path, &config_path).await {
                Ok(pid) => Response::Started { pid },
                Err(e) => Response::Error { message: e },
            }
        }
        Request::Stop => match singbox.stop().await {
            Ok(()) => Response::Stopped,
            Err(e) => Response::Error { message: e },
        },
        Request::Status => {
            let (running, pid, uptime) = singbox.status().await;
            Response::Status { running, pid, uptime_secs: uptime }
        }
        Request::Uninstall => {
            // Stop sing-box, signal that helper should be torn down by external installer
            let _ = singbox.stop().await;
            Response::Uninstalling
        }
    }
}

async fn write_response(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    resp: &Response,
) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string(resp)?;
    w.write_all(json.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await?;
    Ok(())
}
