#!/usr/bin/env python3
"""Linux isolated OMP VM-login transport check (requires sudo, sshd, ip).

Run as the existing local ssf user, after cargo build: python3 scripts/test-omp-vm-login.py target/debug/ssf
All SSH keys, guest state and callbacks are synthetic and temporary. A private
network namespace supplies guest loopback; no live VM or factory is contacted.
"""
import contextlib
import json
import os
from pathlib import Path
import pty
import select
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/ssf").resolve())

def run(*args):
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for(predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.05)
    raise AssertionError("isolated fixture timed out")


with tempfile.TemporaryDirectory(prefix="ssf-omp-login-") as directory:
    root = Path(directory)
    vm = root / "vms" / "test"
    vm.mkdir(parents=True)
    callback_port = free_port()
    ssh_port = free_port()
    for key in [vm / "id_ed25519", root / "host_key"]:
        run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key))
    runner = root / "guest.py"
    runner.write_text('''import http.server, os, pathlib
root = pathlib.Path(__file__).parent
command = os.environ.get("SSH_ORIGINAL_COMMAND", "")
if command == "omp":
    class Callback(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == "/launch":
                self.send_response(302)
                self.send_header("Location", "/callback?code=synthetic")
                self.end_headers()
            elif self.path == "/callback?code=synthetic":
                import sqlite3
                database = root / ".omp" / "agent" / "agent.db"
                database.parent.mkdir(parents=True, exist_ok=True)
                with sqlite3.connect(database) as db:
                    db.execute("CREATE TABLE IF NOT EXISTS auth_credentials (provider TEXT, credential_type TEXT, data TEXT, disabled_cause TEXT)")
                    db.execute("INSERT INTO auth_credentials VALUES ('zai', 'oauth', ?, NULL)", ('{"access":"synthetic-access","refresh":"synthetic-refresh"}',))
                (root / "credential").touch()
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"enrolled")
            else:
                self.send_error(404)
        def log_message(self, *args):
            pass
    server = http.server.HTTPServer(("127.0.0.1", PORT), Callback)
    print("Open http://local\\x1b[32mhost:PORT/launch\\x1b[0m", flush=True)
    while not (root / "credential").exists():
        server.handle_request()
    import sys
    sys.stdin.readline()
elif "login-probe" in command or "auth.json" in command:
    import subprocess
    environment = dict(os.environ, HOME=str(root), SSF_CONFIG_DIR=str(root), SSF_STATE_DIR=str(root / "state"))
    environment.pop("SSF_SERVER", None)
    raise SystemExit(subprocess.run([BINARY, "login-probe", "omp"], env=environment).returncode)
elif command == "printf R; cat >/dev/null":
    print("R", end="", flush=True)
    import sys
    sys.stdin.read()
else:
    raise SystemExit(0)
'''.replace("PORT", str(callback_port)).replace("BINARY", repr(binary)))
    configuration = root / "sshd_config"
    configuration.write_text(f"""Port {ssh_port}
ListenAddress 127.0.0.1
HostKey {root / 'host_key'}
PidFile {root / 'sshd.pid'}
AuthorizedKeysFile {vm / 'id_ed25519.pub'}
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM yes
AllowUsers ssf
AllowTcpForwarding local
PermitOpen localhost:{callback_port}
ForceCommand /usr/bin/python3 {runner}
LogLevel QUIET
""")
    holder = root / "namespace.py"
    holder.write_text('''import os, pathlib, signal, subprocess, time
root = pathlib.Path(__file__).parent
subprocess.run(["ip", "link", "set", "lo", "up"], check=True)
server = subprocess.Popen(["/usr/sbin/sshd", "-D", "-f", str(root / "sshd_config")])
(root / "namespace.pid").write_text(str(os.getpid()))
def stop(*args):
    server.terminate()
    server.wait()
    raise SystemExit(0)
signal.signal(signal.SIGTERM, stop)
while True:
    time.sleep(1)
''')
    namespace = subprocess.Popen(["sudo", "-n", "unshare", "-n", "python3", str(holder)])
    bridge = socket.socket()
    bridge.bind(("127.0.0.1", ssh_port))
    bridge.listen()
    bridge.settimeout(.2)
    stopping = threading.Event()
    connections = []
    try:
        wait_for(lambda: (root / "namespace.pid").exists())
        namespace_pid = (root / "namespace.pid").read_text()
        pipe_code = '''import os, select, socket
s=socket.create_connection(("127.0.0.1", PORT))
while True:
    ready,_,_=select.select([s,0],[],[])
    for fd in ready:
        data=s.recv(65536) if fd is s else os.read(0,65536)
        if not data: raise SystemExit(0)
        s.sendall(data) if fd == 0 else os.write(1,data)
'''.replace("PORT", str(ssh_port))
        def serve():
            while not stopping.is_set():
                try:
                    connection, _ = bridge.accept()
                except socket.timeout:
                    continue
                child = subprocess.Popen(["sudo", "-n", "nsenter", "-t", namespace_pid,
                    "-n", "python3", "-c", pipe_code], stdin=connection, stdout=connection,
                    stderr=subprocess.DEVNULL)
                connections.append(child)
                connection.close()
        server_thread = threading.Thread(target=serve)
        server_thread.start()
        time.sleep(.3)
        probe = subprocess.run(["ssh", "-F", "/dev/null", "-i", str(vm / "id_ed25519"), "-p", str(ssh_port), "-o", "StrictHostKeyChecking=no", "-o", f"UserKnownHostsFile={vm / 'known_hosts'}", "ssf@127.0.0.1", "true"], capture_output=True)
        assert probe.returncode == 0, probe.stderr.decode()
        (root / "config.toml").write_text(f'[vm]\nenabled = true\nbackend = "firecracker"\nname = "test"\ndir = {json.dumps(str(root / "vms"))}\nssh_port = {ssh_port}\n')
        env = dict(os.environ, SSF_CONFIG_DIR=str(root), SSF_STATE_DIR=str(root / "state"))
        env.pop("SSF_VM_GUEST", None)
        env.pop("SSF_SERVER", None)
        def login(collision=False):
            master, slave = pty.openpty()
            before = __import__('termios').tcgetattr(slave)
            child = subprocess.Popen([binary, "vm", "login", "omp"], env=env,
                stdin=slave, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            output = bytearray()
            try:
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    if select.select([child.stdout], [], [], .1)[0]:
                        chunk = os.read(child.stdout.fileno(), 4096)
                        output.extend(chunk)
                        if f":{callback_port}/launch".encode() in output and not collision:
                            import http.client
                            ipv6 = http.client.HTTPConnection("::1", callback_port, timeout=5)
                            ipv6.request("GET", "/launch")
                            response = ipv6.getresponse()
                            assert response.status == 302
                            response.read()
                            ipv6.close()
                            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                            assert opener.open(f"http://127.0.0.1:{callback_port}/launch", timeout=5).read() == b"enrolled"
                            os.write(master, b"exit\n")
                            break
                    if child.poll() is not None:
                        break
                child.wait(timeout=10)
                output.extend(child.stdout.read())
                assert __import__('termios').tcgetattr(slave) == before, "terminal state not restored"
                if collision:
                    assert b"cannot forward OMP OAuth port" in output
                    assert not (root / "credential").exists()
                else:
                    assert child.returncode == 0, "login failed: " + output.decode(errors="replace")
                    assert (root / "credential").exists()
                    assert b"logged in" in output, "credential result missing"
            finally:
                if child.poll() is None:
                    child.kill()
                child.wait()
                os.close(master)
                os.close(slave)
        login()
        for host in ["127.0.0.1", "::1"]:
            with socket.socket(socket.AF_INET6 if ':' in host else socket.AF_INET) as check:
                check.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                check.bind((host, callback_port))
        (root / "credential").unlink()
        (root / ".omp" / "agent" / "agent.db").unlink()
        for host in ["127.0.0.1", "::1"]:
            with socket.socket(socket.AF_INET6 if ':' in host else socket.AF_INET) as occupied:
                occupied.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                occupied.bind((host, callback_port))
                occupied.listen()
                login(collision=True)
        print("PASS: isolated VM login, synthetic OAuth callback, credential result, listener cleanup, port collision and terminal restoration")
    finally:
        stopping.set()
        if 'server_thread' in locals():
            server_thread.join(timeout=2)
        bridge.close()
        if (root / "namespace.pid").exists():
            subprocess.run(["sudo", "-n", "kill", "-TERM", (root / "namespace.pid").read_text()], check=False)
        namespace.wait(timeout=10)
        for child in connections:
            with contextlib.suppress(subprocess.TimeoutExpired):
                child.wait(timeout=2)
