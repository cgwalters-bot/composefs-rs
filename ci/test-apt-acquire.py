"""Exercise native apt transport recovery, never installing into the host.

Run with python3 ci/test-apt-acquire.py. Requires apt-get and dpkg-deb;
uses only Python's standard library and loopback, without root or credentials.
"""

import hashlib
import http.server
import os
import pwd
from pathlib import Path
import posixpath
import socket
import subprocess
import tempfile
import threading
import time


# Threat: an archive peer closes or stalls a connection before setup completes.
# Native acquisition retries do not rerun the solver or package installation.
RETRIES = 2
TIMEOUT = 15
# HTTP can reconnect once within each acquisition attempt. Include both sockets
# and native exponential retry delays (1 + 2 seconds), plus scheduling slack.
MAX_REQUESTS = 2 * (RETRIES + 1)
COMMAND_BUDGET = MAX_REQUESTS * TIMEOUT + 3 + 15
OPTIONS = [
    "-o", f"Acquire::Retries={RETRIES}",
    "-o", f"Acquire::http::Timeout={TIMEOUT}",
    "-o", f"Acquire::https::Timeout={TIMEOUT}",
    "-o", "APT::Update::Error-Mode=any",
]


class Archive(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        server = self.server
        path = posixpath.normpath(self.path)
        target = path == server.target
        if target:
            server.requests += 1
        fail = target and (
            server.mode in ("eof", "timeout")
            or (server.mode == "transient" and server.requests <= 2)
        )
        if fail:
            if server.mode == "timeout":
                server.stop.wait(COMMAND_BUDGET * 2)
            else:
                # EOF while reading a response, rather than HTTP error status.
                self.wfile.write(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n")
                self.wfile.flush()
            self.close_connection = True
            self.connection.shutdown(socket.SHUT_RDWR)
            return
        data = server.files.get(path)
        if data is None:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def main():
    print(subprocess.check_output(["apt-get", "--version"], text=True).splitlines()[0])
    with tempfile.TemporaryDirectory(prefix="apt-acquire-") as directory:
        root = Path(directory)
        package = root / "package"
        (package / "DEBIAN").mkdir(parents=True)
        (package / "DEBIAN/control").write_text(
            "Package: acquisition-fixture\nVersion: 1\nArchitecture: all\n"
            "Maintainer: Test <test@example.invalid>\nDescription: acquisition fixture\n"
        )
        deb = root / "fixture.deb"
        subprocess.run(["dpkg-deb", "--root-owner-group", "--build", str(package), str(deb)],
                       check=True, stdout=subprocess.DEVNULL)
        payload = deb.read_bytes()
        index = (
            "Package: acquisition-fixture\nVersion: 1\nArchitecture: all\n"
            "Filename: fixture.deb\n"
            f"Size: {len(payload)}\nSHA256: {hashlib.sha256(payload).hexdigest()}\n"
            "Description: acquisition fixture\n\n"
            "Package: conflict-fixture\nVersion: 1\nArchitecture: all\n"
            "Depends: acquisition-fixture (= 2)\nDescription: impossible dependency\n\n"
        ).encode()
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Archive)
        server.daemon_threads = True
        server.files = {"/Packages": index, "/fixture.deb": payload}
        server.mode = "healthy"
        server.target = "/fixture.deb"
        server.requests = 0
        server.stop = threading.Event()
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            run_cases(root, server, payload)
        finally:
            server.stop.set()
            server.shutdown()
            server.server_close()
            thread.join()


def run_cases(root, server, payload):
    for name in ("lists/partial", "archives/partial", "empty"):
        (root / name).mkdir(parents=True)
    (root / "status").touch()
    (root / "sources.list").write_text(
        f"deb [trusted=yes] http://127.0.0.1:{server.server_port}/ ./\n"
    )
    config = root / "apt.conf"
    # APT_CONFIG is read before apt.conf.d; disable host config, hooks and
    # sources. All writable state is disposable. Trust applies only to loopback.
    config.write_text(
        f'Dir::Etc::Parts "{root}/empty";\n'
        f'Dir::Etc::main "{root}/absent";\n'
        f'Dir::Etc::sourcelist "{root}/sources.list";\n'
        f'Dir::Etc::sourceparts "{root}/empty";\n'
        f'Dir::State "{root}";\n'
        f'Dir::State::status "{root}/status";\n'
        f'Dir::State::lists "{root}/lists";\n'
        f'Dir::Cache "{root}";\n'
        f'Dir::Cache::archives "{root}/archives";\n'
        'Acquire::http::Proxy "DIRECT";\n'
        'Acquire::Languages "none";\n'
        'Acquire::IndexTargets::deb::DEP-11::DefaultEnabled "false";\n'
        f'APT::Sandbox::User "{pwd.getpwuid(os.getuid()).pw_name}";\n'
    )
    env = {**os.environ, "APT_CONFIG": str(config), "LC_ALL": "C"}

    def apt(label, arguments, mode, code, minimum, maximum, options=OPTIONS,
            diagnostic=None):
        server.mode = mode
        server.requests = 0
        start = time.monotonic()
        try:
            result = subprocess.run(
                ["apt-get", *options, *arguments], env=env, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                timeout=COMMAND_BUDGET,
            )
        except subprocess.TimeoutExpired:
            raise AssertionError(f"{label}: exceeded {COMMAND_BUDGET}s budget") from None
        elapsed = time.monotonic() - start
        if result.returncode != code or (diagnostic and diagnostic not in result.stdout):
            for line in result.stdout.splitlines():
                if line.startswith("E:"):
                    print(line.replace(str(root), "<temporary-root>")
                          .replace(str(server.server_port), "<port>"))
        # Do not print apt output: host configuration/URLs must not leak into CI.
        check(result.returncode == code,
              f"{label}: exit {result.returncode}, expected {code} (output withheld)")
        check(minimum <= server.requests <= maximum,
              f"{label}: requests={server.requests}, expected {minimum}..{maximum}")
        if diagnostic:
            check(diagnostic in result.stdout, f"{label}: missing expected diagnostic")
        print(f"{label}: exit={result.returncode} requests={server.requests} "
              f"elapsed={elapsed:.1f}s budget={COMMAND_BUDGET}s")

    apt("seed indexes", ["update"], "healthy", 0, 0, 0)
    install = ["install", "--download-only", "-y", "acquisition-fixture"]
    # Negative control: two EOFs consume HTTP's internal reconnect, so success
    # below requires Acquire::Retries, not just the internal reconnect.
    apt("no acquisition retries", install, "transient", 100, 2, 2,
        options=[*OPTIONS, "-o", "Acquire::Retries=0"], diagnostic="Failed to fetch")
    apt("transient EOF then success", install, "transient", 0, 3, MAX_REQUESTS)
    archive = root / "archives/acquisition-fixture_1_all.deb"
    check(archive.read_bytes() == payload, "downloaded archive differs from fixture")
    archive.unlink()
    for mode in ("eof", "timeout"):
        start = time.monotonic()
        apt(f"persistent {mode}", install, mode, 100, MAX_REQUESTS, MAX_REQUESTS,
            diagnostic="Failed to fetch")
        if mode == "timeout":
            check(time.monotonic() - start >= (RETRIES + 1) * TIMEOUT,
                  "timeout failed without exercising the idle timeout")
    apt("missing package", ["install", "--download-only", "-y", "absent-fixture"],
        "healthy", 100, 0, 0, diagnostic="Unable to locate package")
    apt("dependency conflict", ["install", "--download-only", "-y", "conflict-fixture"],
        "healthy", 100, 0, 0, diagnostic="Depends: acquisition-fixture (= 2)")
    # Preserve cached indexes, then fail their refresh. Default update can exit
    # zero in this situation; Error-Mode=any must prevent downstream installs.
    server.target = "/Packages"
    apt("cached index refresh failure", ["update"], "eof", 100, 1, MAX_REQUESTS,
        diagnostic="Failed to fetch")
    check((root / "status").read_bytes() == b"", "host-style install occurred")


if __name__ == "__main__":
    main()
