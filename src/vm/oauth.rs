//! Session-scoped forwarding for OMP's short loopback OAuth launch links.
use super::*;
use std::io::Read;
use std::process::Child;

/// SSH owns raw mode during the TUI; restore it even if an error kills SSH.
pub(super) struct TerminalRestore(Option<libc::termios>);

impl TerminalRestore {
    pub(super) fn capture() -> Self {
        let mut state = std::mem::MaybeUninit::uninit();
        // SAFETY: tcgetattr initializes the provided termios on success.
        let saved = unsafe {
            (libc::tcgetattr(libc::STDIN_FILENO, state.as_mut_ptr()) == 0)
                .then(|| state.assume_init())
        };
        Self(saved)
    }
}

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        if let Some(state) = self.0.as_ref() {
            // SAFETY: state was initialized by tcgetattr on this descriptor.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, state);
            }
        }
    }
}

/// Kill on every exit path, including terminal I/O errors. Dropping Child alone
/// leaves the SSH process (and its callback listeners) running.
pub(super) struct SessionProcess(pub Child);

impl Drop for SessionProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) struct CallbackTunnel {
    port: u16,
    process: SessionProcess,
}

impl CallbackTunnel {
    pub(super) fn start(vm: &Vm, port: u16) -> Result<Self> {
        let mut command = Command::new("ssh");
        command.args(["-F", "/dev/null", "-o", "IdentitiesOnly=yes"]);
        command.args(vm.ssh_args(true));
        command.args(["-T", "-o", "ControlMaster=no", "-o", "ControlPath=none"]);
        command.args(["-o", "ExitOnForwardFailure=yes", "-o", "GatewayPorts=no"]);
        // Explicit listeners avoid OpenSSH silently accepting only one address
        // family when the other is occupied. Nothing binds a public interface.
        for address in ["127.0.0.1", "[::1]"] {
            command
                .arg("-L")
                .arg(format!("{address}:{port}:localhost:{port}"));
        }
        command
            .arg(vm.target())
            .arg("--")
            .arg("printf R; cat >/dev/null");
        let mut process = SessionProcess(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .context("starting OMP callback forwarding")?,
        );
        let mut output = process.0.stdout.take().expect("piped stdout");
        let (send, receive) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut marker = [0];
            let ready = output.read_exact(&mut marker).is_ok() && marker == *b"R";
            let _ = send.send(ready);
        });
        let ready = receive
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or(false);
        if !ready {
            // Close the pipe reader before joining on a stalled connection.
            let _ = process.0.kill();
        }
        let _ = reader.join();
        if !ready {
            bail!(
                "cannot forward OMP OAuth port {port} on host loopback (IPv4 and IPv6); free that port and retry `ssf vm login omp`"
            );
        }
        Ok(Self { port, process })
    }

    pub(super) fn port(&self) -> u16 {
        self.port
    }

    pub(super) fn check(&mut self) -> Result<()> {
        if self.process.0.try_wait()?.is_some() {
            bail!("OMP callback forwarding stopped; retry `ssf vm login omp`");
        }
        Ok(())
    }
}

/// A bounded parser: recognizes only OMP's launch link, never callback query
/// strings. CSI styling can split the link anywhere; OSC hyperlink metadata is
/// ignored, so it cannot request additional forwarding.
#[derive(Default)]
pub(super) struct LaunchScanner {
    text: Vec<u8>,
    escape: u8,
    pending: Option<u16>,
}

impl LaunchScanner {
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Vec<u16> {
        let mut ports = Vec::new();
        for &byte in bytes {
            match self.escape {
                1 => {
                    self.escape = match byte {
                        b'[' => 2,
                        b']' => 3,
                        _ => 0,
                    };
                    continue;
                }
                2 => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.escape = 0;
                    }
                    continue;
                }
                3 => {
                    if byte == 7 {
                        self.escape = 0;
                    } else if byte == 27 {
                        self.escape = 4;
                    }
                    continue;
                }
                4 => {
                    self.escape = if byte == b'\\' { 0 } else { 3 };
                    continue;
                }
                _ => {}
            }
            if byte == 27 {
                self.escape = 1;
                continue;
            }
            if let Some(port) = self.pending.take()
                && byte.is_ascii_whitespace()
                && !ports.contains(&port)
            {
                ports.push(port);
            }
            if byte.is_ascii_whitespace() {
                self.text.clear();
                continue;
            }
            self.text.push(byte);
            if self.text.len() > 80 {
                self.text.remove(0);
            }
            if self.text.ends_with(b"/launch") {
                for prefix in [
                    b"http://localhost:".as_slice(),
                    b"http://127.0.0.1:".as_slice(),
                    b"http://[::1]:".as_slice(),
                ] {
                    if let Some(start) = self.text.windows(prefix.len()).rposition(|s| s == prefix)
                    {
                        let digits = &self.text[start + prefix.len()..self.text.len() - 7];
                        if !digits.is_empty()
                            && digits.iter().all(u8::is_ascii_digit)
                            && let Ok(port) =
                                std::str::from_utf8(digits).unwrap_or("").parse::<u16>()
                            && port >= 1024
                            && !ports.contains(&port)
                        {
                            self.pending = Some(port);
                        }
                    }
                }
                self.text.clear();
            }
        }
        ports
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_link_survives_chunk_boundaries_and_tui_styling() {
        let text = b"Open http://local\x1b[32mhost:432\x1b[0m10/launch\r\n";
        for split in 0..text.len() {
            let mut scanner = LaunchScanner::default();
            let mut ports = scanner.feed(&text[..split]);
            ports.extend(scanner.feed(&text[split..]));
            assert_eq!(ports, [43210]);
        }
    }

    #[test]
    fn ignores_callbacks_external_urls_privileged_ports_and_hyperlink_metadata() {
        let mut scanner = LaunchScanner::default();
        assert!(scanner.feed(b"http://localhost:43210/callback?code=secret http://evil:43210/launch http://localhost:43210/launch?code=secret http://localhost:43210/launch#fragment http://localhost:22/launch http://localhost:99999/launch \x1b]8;;http://localhost:43210/launch\x1b\\label\x1b]8;;\x1b\\").is_empty());
        assert!(scanner.text.len() <= 80);
        assert_eq!(scanner.feed(b" http://127.0.0.1:12345/launch "), [12345]);
    }
}
