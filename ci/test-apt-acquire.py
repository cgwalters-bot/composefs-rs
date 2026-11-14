"""Test native apt acquisition recovery using only disposable state and loopback.

Run explicitly with Python 3; requires apt-get and dpkg-deb, but not root.
Packages are downloaded, never installed. No external archive is contacted.
"""

import hashlib
import http.server
import os
from pathlib import Path
import posixpath
import pwd
import socket
import subprocess
import tempfile
import threading
import time


# Threat: the archive closes or stalls a connection before example setup finishes.
# Use apt's acquisition retries, not a retry of solving/installing packages.
RETRIES = 2
IDLE_TIMEOUT = 15
# HTTP reconnects once per acquisition attempt, then apt backs off 1 + 2s.
MAX_REQUESTS = 2 * (RETRIES + 1)
COMMAND_BUDGET = MAX_REQUESTS * IDLE_TIMEOUT + 3 + 15
OPTIONS = [
    "-o", f"Acquire::Retries={RETRIES}",
    "-o", f"Acquire::http::Timeout={IDLE_TIMEOUT}",
    "-o", f"Acquire::https::Timeout={IDLE_TIMEOUT}",
    "-o", "APT::Update::Error-Mode=any",
]


def check(condition, message):
    if not condition:
        raise AssertionError(message)


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
                # Close in the response headers, not an HTTP error response.
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
        subprocess.run(
            ["dpkg-deb", "--root-owner-group", "--build", str(package), str(deb)],
            check=True, stdout=subprocess.DEVNULL,
        )
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
    for name in ("lists/partial", "archives/partial", "empty", "etc"):
        (root / name).mkdir(parents=True)
    (root / "status").touch()
    (root / "etc/sources.list").write_text(
        f"deb [trusted=yes] http://127.0.0.1:{server.server_port}/ ./\n"
    )
    config = root / "apt.conf"
    # APT_CONFIG loads before the host config. Redirect the entire etc tree,
    # including preferences and auth, not just sources and apt.conf.d hooks.
    config.write_text(
        f'Dir::Etc "{root}/etc";\n'
        f'Dir::Etc::Parts "{root}/empty";\n'
        f'Dir::Etc::main "{root}/absent";\n'
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
    # Do not inherit APT_CONFIG, proxy auto-configuration or locale from callers.
    env = {"PATH": os.environ["PATH"], "APT_CONFIG": str(config), "LC_ALL": "C"}

    def apt(label, arguments, mode, code, requests, options=OPTIONS, diagnostic=None):
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
        if result.returncode != code:
            for line in result.stdout.splitlines():
                if line.startswith("E:"):
                    print(line.replace(str(root), "<temporary-root>")
                          .replace(str(server.server_port), "<port>"))
        # Withhold raw apt output, which might contain environment-specific data.
        check(result.returncode == code,
              f"{label}: exit {result.returncode}, expected {code} (output withheld)")
        check(server.requests == requests,
              f"{label}: requests={server.requests}, expected {requests}")
        if diagnostic:
            check(diagnostic in result.stdout, f"{label}: missing expected diagnostic")
        print(f"{label}: exit={result.returncode} requests={server.requests} "
              f"elapsed={elapsed:.1f}s budget={COMMAND_BUDGET}s")
        return elapsed

    apt("seed indexes", ["update"], "healthy", 0, 0)
    install = ["install", "--download-only", "-y", "acquisition-fixture"]
    # Two EOFs exhaust HTTP's reconnect; recovery must need acquisition retries.
    apt("no acquisition retries", install, "transient", 100, 2,
        options=[*OPTIONS, "-o", "Acquire::Retries=0"], diagnostic="Failed to fetch")
    apt("transient EOF then success", install, "transient", 0, 3)
    archive = root / "archives/acquisition-fixture_1_all.deb"
    check(archive.read_bytes() == payload, "downloaded archive differs from fixture")
    archive.unlink()
    for mode in ("eof", "timeout"):
        elapsed = apt(f"persistent {mode}", install, mode, 100, MAX_REQUESTS,
                      diagnostic="Failed to fetch")
        if mode == "timeout":
            check(elapsed >= (RETRIES + 1) * IDLE_TIMEOUT,
                  "timeout failed without exercising the idle timeout")
    apt("missing package", ["install", "--download-only", "-y", "absent-fixture"],
        "healthy", 100, 0, diagnostic="Unable to locate package")
    apt("dependency conflict", ["install", "--download-only", "-y", "conflict-fixture"],
        "healthy", 100, 0, diagnostic="Depends: acquisition-fixture (= 2)")
    # Keep the successful update's cached indexes, then fail their refresh.
    server.target = "/Packages"
    apt("cached index refresh failure", ["update"], "eof", 100, MAX_REQUESTS,
        diagnostic="Failed to fetch")
    check((root / "status").read_bytes() == b"", "package installation occurred")


if __name__ == "__main__":
    main()
