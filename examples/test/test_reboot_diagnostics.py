"""Reboot evidence tests: no guest, SSH server or generated key fixture needed."""

import asyncio
import os
import threading
from pathlib import Path
from unittest.mock import AsyncMock, Mock

import pytest
import testthing


@pytest.fixture
def vm(tmp_path: Path) -> testthing.VirtualMachine:
    machine = testthing.VirtualMachine(
        "unused.qcow2", ipc=tmp_path, reboot_diagnostics=True
    )
    machine._ssh_control_task = Mock()
    machine._ssh_control_ready.set()
    return machine


def command(
    vm: testthing.VirtualMachine, error: BaseException | None = None
) -> AsyncMock:
    async def execute(*args: object, **kwargs: object) -> str:
        assert args == ("reboot",)
        assert kwargs == {"direct": True}
        vm._ssh_control_ready.set()
        if error is not None:
            raise error
        return ""

    vm.execute = AsyncMock(side_effect=execute)
    return vm.execute


async def test_command_failure_before_ipc_cleanup(
    vm: testthing.VirtualMachine,
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    caplog: pytest.LogCaptureFixture,
) -> None:
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))
    error = testthing.SubprocessError(("ssh", "reboot"), 255)
    execute = command(vm, error)
    with testthing.IpcDirectory() as ipc:
        vm._ipc = ipc
        (ipc / "console").write_text("[ OK ] Reached target multi-user\n")
        with pytest.raises(testthing.SubprocessError) as caught:
            await vm.reboot()
        assert caught.value is error
        assert "Reached target multi-user" in caplog.text
    assert not ipc.exists()
    execute.assert_awaited_once()
    assert "phase=direct-ssh failed" in caplog.text
    assert "returncode=255" in caplog.text
    assert "phase=reconnect complete" in caplog.text
    assert "elapsed=" in caplog.text


@pytest.mark.parametrize(
    "error_type", [RuntimeError, asyncio.CancelledError, TimeoutError]
)
async def test_reconnect_failure(
    vm: testthing.VirtualMachine,
    caplog: pytest.LogCaptureFixture,
    error_type: type[BaseException],
) -> None:
    execute = command(vm)
    error = error_type("primary")
    vm._ssh_control_ready.wait = AsyncMock(side_effect=error)
    with pytest.raises(error_type) as caught:
        await vm.reboot()
    assert caught.value is error
    execute.assert_awaited_once()
    assert "phase=direct-ssh complete" in caplog.text
    assert "phase=reconnect failed" in caplog.text
    assert "console unavailable error=FileNotFoundError" in caplog.text
    assert vm._reboot_started is None


async def test_real_reconnect_cancellation(
    vm: testthing.VirtualMachine,
    caplog: pytest.LogCaptureFixture,
) -> None:
    waiting = asyncio.Event()

    async def execute(*args: object, **kwargs: object) -> str:
        waiting.set()
        return ""

    vm.execute = AsyncMock(side_effect=execute)
    task = asyncio.create_task(vm.reboot())
    await waiting.wait()
    # A guest which never comes back must not be accepted as a successful boot.
    assert not task.done()
    task.cancel("original cancellation")
    with pytest.raises(asyncio.CancelledError, match="original cancellation"):
        await task
    assert task.cancelled()
    assert "phase=reconnect failed" in caplog.text


@pytest.mark.parametrize("enabled", [False, True])
async def test_success(
    vm: testthing.VirtualMachine,
    caplog: pytest.LogCaptureFixture,
    enabled: bool,
) -> None:
    vm._reboot_diagnostics = enabled
    execute = command(vm)
    await vm.reboot()
    execute.assert_awaited_once()
    assert ("phase=direct-ssh complete" in caplog.text) is enabled
    assert ("phase=reconnect complete" in caplog.text) is enabled
    assert "console" not in caplog.text
    assert vm._reboot_started is None


async def test_default_is_off(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    vm = testthing.VirtualMachine("unused", ipc=tmp_path)
    vm._ssh_control_task = Mock()
    vm._ssh_control_ready.set()
    error = RuntimeError("primary")
    command(vm, error)
    with pytest.raises(RuntimeError) as caught:
        await vm.reboot()
    assert caught.value is error
    assert not caplog.records


@pytest.mark.parametrize("broken", ["read", "sanitize", "log"])
@pytest.mark.parametrize("cancelled", [False, True])
async def test_diagnostic_failure_preserves_primary(
    vm: testthing.VirtualMachine,
    monkeypatch: pytest.MonkeyPatch,
    broken: str,
    cancelled: bool,
) -> None:
    error = (
        asyncio.CancelledError("primary")
        if cancelled
        else testthing.SubprocessError(("ssh", "reboot"), 255)
    )
    command(vm, error)
    (vm._ipc / "console").write_text("boot evidence\n")
    failure = Mock(side_effect=OSError("secondary diagnostic failure"))
    if broken == "read":
        monkeypatch.setattr(testthing, "_console_tail", failure)
    elif broken == "sanitize":
        monkeypatch.setattr(testthing, "_sanitize_console", failure)
    else:
        monkeypatch.setattr(testthing.logger, "warning", failure)
    with pytest.raises(type(error)) as caught:
        await vm.reboot()
    assert caught.value is error
    assert failure.called


async def test_bounded_redacted_tail(
    vm: testthing.VirtualMachine,
    caplog: pytest.LogCaptureFixture,
) -> None:
    # Values are ephemeral; even PEM boundary strings are assembled at runtime.
    secret = os.urandom(24).hex()
    public = "ssh-ed25519 " + os.urandom(32).hex()
    vm._credentials = {"example": secret}
    vm._identity = (vm._ipc / "id", public)
    marker = "-----BEGIN " + "OPENSSH PRIVATE KEY-----"
    console = vm._ipc / "console"
    console.write_text(
        "old evidence\n"
        + "x" * (testthing._REBOOT_CONSOLE_BYTES * 2)
        + "\n"
        + secret
        + "\n"
        + public
        + "\n"
        + marker
        + "\n"
        + "A" * 64
        + "\n-----END "
        + "OPENSSH PRIVATE KEY-----\n"
        + "password=hidden\n\x1b[31m[FAILED] boot target\x1b[0m\n"
    )
    data = testthing._console_tail(vm._ipc)
    assert len(data) <= testthing._REBOOT_CONSOLE_BYTES
    assert b"old evidence" not in data
    command(vm, RuntimeError("primary"))
    with pytest.raises(RuntimeError):
        await vm.reboot()
    assert "[FAILED] boot target" in caplog.text
    assert "[redacted]" in caplog.text
    for value in (secret, public, marker, "A" * 64, "password=hidden", "\x1b"):
        assert value not in caplog.text
    tail = next(r.args[1] for r in caplog.records if "console tail" in r.msg)
    assert len(tail.encode()) <= testthing._REBOOT_CONSOLE_BYTES


@pytest.mark.parametrize("kind", ["absent", "fifo", "symlink"])
async def test_unavailable_console(
    vm: testthing.VirtualMachine,
    caplog: pytest.LogCaptureFixture,
    kind: str,
) -> None:
    console = vm._ipc / "console"
    if kind == "fifo":
        os.mkfifo(console)
    elif kind == "symlink":
        target = vm._ipc / "id"
        target.write_text("must not read identity file")
        console.symlink_to(target)
    command(vm, RuntimeError("primary"))
    with pytest.raises(RuntimeError, match="primary"):
        await vm.reboot()
    assert "console unavailable" in caplog.text
    assert "must not read" not in caplog.text


@pytest.mark.parametrize("broken", ["read", "log"])
async def test_stalled_diagnostic_is_bounded(
    vm: testthing.VirtualMachine,
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
    broken: str,
) -> None:
    release = threading.Event()
    entered = threading.Event()

    def stall(*args: object) -> bytes:
        entered.set()
        release.wait()
        return b"late evidence must not be logged"

    if broken == "read":
        monkeypatch.setattr(testthing, "_console_tail", stall)
    else:
        monkeypatch.setattr(testthing.logger, "warning", stall)
    error = RuntimeError("primary")
    command(vm, error)
    try:
        # Even all five phase logs plus the console worker stalling are finite.
        with pytest.raises(RuntimeError) as caught:
            await asyncio.wait_for(
                vm.reboot(), 8 * testthing._REBOOT_DIAGNOSTIC_SECONDS
            )
        assert entered.is_set()
    finally:
        release.set()
    assert caught.value is error
    assert "late evidence" not in caplog.text
    assert 0 < testthing._REBOOT_DIAGNOSTIC_SECONDS <= 1
    assert 0 < testthing._REBOOT_CONSOLE_BYTES <= 16384


@pytest.mark.parametrize("include_begin", [False, True])
async def test_prefixed_private_key_redaction(
    vm: testthing.VirtualMachine,
    caplog: pytest.LogCaptureFixture,
    include_begin: bool,
) -> None:
    begin = "-----BEGIN " + "OPENSSH PRIVATE KEY-----"
    end = "-----END " + "OPENSSH PRIVATE KEY-----"
    body = os.urandom(48).hex()
    lines = ([f"service[42]: {begin}"] if include_begin else []) + [
        f"service[42]: {body}",
        "service[42]: YQ==",
        f"service[42]: {end}",
        "[FAILED] boot target",
    ]
    (vm._ipc / "console").write_text("\n".join(lines))
    command(vm, RuntimeError("primary"))
    with pytest.raises(RuntimeError, match="primary"):
        await vm.reboot()
    assert "[FAILED] boot target" in caplog.text
    for value in (begin, end, body, "YQ=="):
        assert value not in caplog.text


async def test_real_execute_exit_255(
    vm: testthing.VirtualMachine,
    caplog: pytest.LogCaptureFixture,
) -> None:
    vm.ssh_direct_args = ("unused-host",)

    async def communicate(data: bytes) -> tuple[bytes, None]:
        assert data == b""
        vm._ssh_control_ready.set()
        return b"command output", None

    process = Mock()
    process.communicate = AsyncMock(side_effect=communicate)
    process.wait = AsyncMock(return_value=255)
    vm._spawn = AsyncMock(return_value=process)
    with pytest.raises(testthing.SubprocessError) as caught:
        await vm.reboot()
    assert caught.value.returncode == 255
    assert caught.value.output == b"command output"
    assert vm._spawn.call_args.args == (
        "ssh",
        "unused-host",
        ("--", "set -eu;"),
        (),
        "reboot",
    )
    assert "phase=direct-ssh failed" in caplog.text
    assert "returncode=255" in caplog.text


async def test_background_failure_during_reconnect(
    vm: testthing.VirtualMachine,
    caplog: pytest.LogCaptureFixture,
) -> None:
    requested = asyncio.Event()
    error = RuntimeError("background failure")

    async def execute(*args: object, **kwargs: object) -> str:
        requested.set()
        return ""

    async def background() -> None:
        await requested.wait()
        raise error

    vm.execute = AsyncMock(side_effect=execute)
    with pytest.raises(ExceptionGroup) as caught:
        async with asyncio.TaskGroup() as tasks:
            tasks.create_task(background())
            await vm.reboot()
    assert caught.value.exceptions == (error,)
    assert "phase=reconnect failed" in caplog.text
    assert "error=CancelledError" in caplog.text
