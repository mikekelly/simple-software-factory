//! The web dashboard of a Firecracker guest on the host's loopback (#653).
//!
//! The guest daemon serves the dashboard on its own loopback address, and
//! gvproxy's user-mode network cannot reach that. So the two ends relay each
//! connection over vsock instead, with no process of their own:
//!
//! * host ([`serve_host`], in the VM supervisor): accept on
//!   `<bind>:<port>`, connect to the VM's vsock socket (`v.sock`), ask for
//!   [`VSOCK_PORT`] with Firecracker's host-initiated `CONNECT <port>\n`,
//!   wait for `OK <n>\n`, then copy both ways.
//! * guest ([`serve_guest`], beside the dashboard listener): accept on
//!   vsock [`VSOCK_PORT`] and copy each connection to the dashboard's own
//!   loopback address.
//!
//! A guest that is down or restarting answers no `OK`, and the host closes
//! that one client connection; the next one tries again. Every relay task
//! belongs to the serving future, so dropping it (the VM run ending, or the
//! guest's `[dashboard]` changing) ends them all.
use anyhow::{Context, Result, bail};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::task::JoinSet;

/// The guest vsock port the dashboard is relayed on; 1024 is gvforwarder's.
pub(crate) const VSOCK_PORT: u32 = 1026;
/// How long the handshake with the guest may take before the client's
/// connection is closed.
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(5);

/// Bind the host's end, naming the port when it is taken.
pub(crate) async fn bind_host(address: SocketAddr) -> Result<TcpListener> {
    TcpListener::bind(address).await.with_context(|| {
        format!(
            "could not listen on {address} for the guest's web dashboard (is another program, or a host-mode ssf-server, using it?)"
        )
    })
}

/// Relay every connection on `listener` to the guest's dashboard through
/// the Firecracker vsock socket `uds`. Never returns; drop it to stop.
pub(crate) async fn serve_host(listener: TcpListener, uds: PathBuf, port: u32) {
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((client, _)) => {
                    let uds = uds.clone();
                    tasks.spawn(async move {
                        if let Err(e) = relay_host(client, &uds, port).await {
                            tracing::debug!("dashboard relay: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!("dashboard relay accept: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            },
            Some(_) = tasks.join_next() => {}
        }
    }
}

async fn relay_host(mut client: TcpStream, uds: &Path, port: u32) -> Result<()> {
    let guest = tokio::time::timeout(HANDSHAKE_LIMIT, connect_guest(uds, port))
        .await
        .context("the guest did not answer in time")??;
    let mut guest = guest;
    tokio::io::copy_bidirectional(&mut client, &mut guest).await?;
    Ok(())
}

/// Firecracker's host-initiated vsock connection: `CONNECT <port>\n`, then
/// `OK <host port>\n` once something in the guest accepted it.
async fn connect_guest(uds: &Path, port: u32) -> Result<BufReader<UnixStream>> {
    let mut stream = UnixStream::connect(uds)
        .await
        .with_context(|| format!("connecting to {}", uds.display()))?;
    stream
        .write_all(format!("CONNECT {port}\n").as_bytes())
        .await?;
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    stream.read_line(&mut line).await?;
    if !line.starts_with("OK ") {
        bail!("the guest refused the dashboard connection ({line:?})");
    }
    Ok(stream)
}

/// Copy one accepted guest-side connection to the dashboard at `target`.
pub(crate) async fn relay_guest<S>(mut stream: S, target: SocketAddr) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut dashboard = TcpStream::connect(target)
        .await
        .with_context(|| format!("connecting to the dashboard at {target}"))?;
    tokio::io::copy_bidirectional(&mut stream, &mut dashboard).await?;
    Ok(())
}

/// The guest's end: accept on vsock [`VSOCK_PORT`] and relay to `target`.
/// Returns only when the vsock listener cannot be made (no vsock device:
/// a lima or Incus guest), which is not an error for the daemon.
pub(crate) async fn serve_guest(target: SocketAddr) -> Result<()> {
    let listener = vsock::Listener::bind(VSOCK_PORT)?;
    tracing::info!("relaying vsock port {VSOCK_PORT} to the web dashboard at {target}");
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    tasks.spawn(async move {
                        if let Err(e) = relay_guest(stream, target).await {
                            tracing::debug!("dashboard relay: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!("dashboard relay accept: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            },
            Some(_) = tasks.join_next() => {}
        }
    }
}

/// A minimal AF_VSOCK listener on libc (Linux only).
mod vsock {
    use anyhow::{Result, bail};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use tokio::io::unix::AsyncFd;
    use tokio::net::UnixStream;

    pub(super) struct Listener(AsyncFd<OwnedFd>);

    impl Listener {
        #[cfg(target_os = "linux")]
        pub(super) fn bind(port: u32) -> Result<Self> {
            // SAFETY: plain socket calls on a descriptor this function owns.
            unsafe {
                let fd = libc::socket(
                    libc::AF_VSOCK,
                    libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                    0,
                );
                if fd < 0 {
                    bail!("vsock socket: {}", std::io::Error::last_os_error());
                }
                let fd = OwnedFd::from_raw_fd(fd);
                let mut address: libc::sockaddr_vm = std::mem::zeroed();
                address.svm_family = libc::AF_VSOCK as libc::sa_family_t;
                address.svm_cid = libc::VMADDR_CID_ANY;
                address.svm_port = port;
                if libc::bind(
                    fd.as_raw_fd(),
                    &address as *const libc::sockaddr_vm as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
                ) < 0
                    || libc::listen(fd.as_raw_fd(), 64) < 0
                {
                    bail!("vsock port {port}: {}", std::io::Error::last_os_error());
                }
                Ok(Self(AsyncFd::new(fd)?))
            }
        }

        #[cfg(not(target_os = "linux"))]
        pub(super) fn bind(_port: u32) -> Result<Self> {
            bail!("vsock is Linux only")
        }

        /// The next connection. A vsock stream socket reads, writes and
        /// shuts down like any stream socket, so it is driven as a
        /// `UnixStream`; nothing here asks it for a Unix address.
        pub(super) async fn accept(&self) -> std::io::Result<UnixStream> {
            loop {
                let mut ready = self.0.readable().await?;
                // SAFETY: accept4 on the listening descriptor; the result
                // is a new descriptor owned by the returned stream.
                let fd = unsafe {
                    libc::accept4(
                        self.0.get_ref().as_raw_fd(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                    )
                };
                if fd >= 0 {
                    // SAFETY: a fresh descriptor from accept4.
                    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
                    return UnixStream::from_std(std::os::unix::net::UnixStream::from(owned));
                }
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    ready.clear_ready();
                    continue;
                }
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// A private directory short enough for a Unix socket path.
    fn scratch() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ssf-relay-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in for Firecracker's `v.sock`: answers `CONNECT <port>` with
    /// `OK`, then echoes; or, `up` false, closes as a guest that is down.
    async fn fake_vsock(dir: &Path, up: bool) -> PathBuf {
        let path = dir.join("v.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut stream = BufReader::new(stream);
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    assert_eq!(line, format!("CONNECT {VSOCK_PORT}\n"));
                    if !up {
                        return;
                    }
                    stream.write_all(b"OK 1073741824\n").await.unwrap();
                    let (mut read, mut write) = tokio::io::split(stream);
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                });
            }
        });
        path
    }

    async fn host_relay(uds: PathBuf) -> SocketAddr {
        let listener = bind_host("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(serve_host(listener, uds, VSOCK_PORT));
        address
    }

    #[tokio::test]
    async fn the_host_relays_through_connect_and_ok() {
        let dir = scratch();
        let address = host_relay(fake_vsock(dir.as_path(), true).await).await;
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        let mut echoed = [0u8; 16];
        client.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"GET / HTTP/1.1\r\n");
    }

    #[tokio::test]
    async fn a_guest_that_is_down_closes_the_client_connection() {
        let dir = scratch();
        // Down: the guest never answers OK.
        let address = host_relay(fake_vsock(dir.as_path(), false).await).await;
        let mut client = TcpStream::connect(address).await.unwrap();
        let mut buf = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut buf))
            .await
            .unwrap();
        assert!(read.is_ok_and(|n| n == 0) || buf.is_empty());
        // No socket at all (the VM stopped): closed too, and the listener
        // keeps serving for when it is back.
        let address = host_relay(dir.as_path().join("missing.sock")).await;
        for _ in 0..2 {
            let mut client = TcpStream::connect(address).await.unwrap();
            let mut buf = Vec::new();
            let n = tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut buf))
                .await
                .unwrap()
                .unwrap_or(0);
            assert_eq!(n, 0);
        }
    }

    #[tokio::test]
    async fn a_host_port_in_use_is_named() {
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let error = bind_host(taken.local_addr().unwrap()).await.unwrap_err();
        let text = format!("{error:#}");
        assert!(
            text.contains(&taken.local_addr().unwrap().to_string()),
            "{text}"
        );
        assert!(text.contains("could not listen"), "{text}");
    }

    #[tokio::test]
    async fn the_guest_relays_a_connection_to_its_dashboard() {
        let dashboard = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = dashboard.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = dashboard.accept().await.unwrap();
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(b"pong").await.unwrap();
        });
        // A socket pair stands in for the accepted vsock connection.
        let (mut outside, inside) = UnixStream::pair().unwrap();
        let relay = tokio::spawn(relay_guest(inside, target));
        outside.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        outside.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
        drop(outside);
        relay.await.unwrap().unwrap();
    }
}
