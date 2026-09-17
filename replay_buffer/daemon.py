"""replayd — background daemon for Android replay capture."""

from __future__ import annotations

import argparse
import os
import signal
import socket
import sys
import threading
import time
from pathlib import Path

from replay_buffer.adb import list_devices, pick_active_device
from replay_buffer.config import Config, load_config
from replay_buffer.ipc import IpcResponse, handle_client
from replay_buffer.session import SessionManager


class ReplayDaemon:
    def __init__(self, config: Config) -> None:
        self.config = config
        self.session_manager = SessionManager(config)
        self._stop_event = threading.Event()
        self._monitor_thread: threading.Thread | None = None
        self._server_socket: socket.socket | None = None
        self._active_serial: str | None = None

    def run_foreground(self) -> int:
        self._prepare_state_dir()
        self._write_pid()
        self._install_signal_handlers()
        self._start_monitor()
        self._start_ipc_server()

        print("Waiting for Android device...", flush=True)
        try:
            while not self._stop_event.is_set():
                time.sleep(0.5)
        finally:
            self._shutdown()
        return 0

    def _prepare_state_dir(self) -> None:
        self.config.state_directory.mkdir(parents=True, exist_ok=True)
        self.config.output_directory.mkdir(parents=True, exist_ok=True)
        if self.config.socket_path.exists():
            self.config.socket_path.unlink()

    def _write_pid(self) -> None:
        self.config.pid_path.write_text(str(os.getpid()), encoding="utf-8")

    def _remove_pid(self) -> None:
        if self.config.pid_path.exists():
            self.config.pid_path.unlink(missing_ok=True)

    def _install_signal_handlers(self) -> None:
        def _handle(signum, _frame) -> None:  # noqa: ANN001
            self._stop_event.set()

        signal.signal(signal.SIGINT, _handle)
        signal.signal(signal.SIGTERM, _handle)

    def _start_monitor(self) -> None:
        self._monitor_thread = threading.Thread(
            target=self._monitor_loop,
            name="adb-monitor",
            daemon=True,
        )
        self._monitor_thread.start()

    def _monitor_loop(self) -> None:
        while not self._stop_event.is_set():
            try:
                device = pick_active_device(self.config.adb_path)
            except Exception as exc:  # noqa: BLE001 - keep daemon alive
                print(f"ADB monitor error: {exc}", file=sys.stderr, flush=True)
                time.sleep(2.0)
                continue

            if device is None:
                if self._active_serial is not None:
                    print("Device disconnected", flush=True)
                    self.session_manager.on_device_disconnected(self._active_serial)
                    self._active_serial = None
                    print("Waiting for Android device...", flush=True)
            elif self._active_serial != device.serial:
                try:
                    session = self.session_manager.on_device_connected(device)
                except Exception as exc:  # noqa: BLE001 - keep daemon alive
                    print(f"Failed to start session: {exc}", file=sys.stderr, flush=True)
                    self._active_serial = device.serial
                    time.sleep(2.0)
                    continue

                self._active_serial = device.serial
                print(f"{session.device_name} connected", flush=True)
                print("● Recording", flush=True)
                print(f"Replay: {self.config.replay_seconds} sec", flush=True)
                print("Logs: logcat", flush=True)

            time.sleep(1.0)

    def _start_ipc_server(self) -> None:
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(str(self.config.socket_path))
        server.listen(5)
        self._server_socket = server

        def _serve() -> None:
            while not self._stop_event.is_set():
                server.settimeout(1.0)
                try:
                    connection, _addr = server.accept()
                except socket.timeout:
                    continue
                except OSError:
                    break
                handle_client(connection, self._handle_request)

        ipc_thread = threading.Thread(target=_serve, name="ipc-server", daemon=True)
        ipc_thread.start()

    def _handle_request(self, payload: dict) -> IpcResponse:
        command = payload.get("command")
        if command == "status":
            return IpcResponse(ok=True, message="ok", data=self.session_manager.status())
        if command == "save":
            try:
                replay_dir = self.session_manager.save()
            except Exception as exc:  # noqa: BLE001 - return to CLI
                return IpcResponse(ok=False, message=str(exc), data={})
            print(f"Saved replay: {replay_dir}", flush=True)
            return IpcResponse(
                ok=True,
                message="saved",
                data={"path": str(replay_dir)},
            )
        if command == "stop":
            self._stop_event.set()
            return IpcResponse(ok=True, message="stopping", data={})
        return IpcResponse(ok=False, message=f"Unknown command: {command}", data={})

    def _shutdown(self) -> None:
        self.session_manager.stop_all()
        if self._server_socket is not None:
            try:
                self._server_socket.close()
            except OSError:
                pass
        if self.config.socket_path.exists():
            self.config.socket_path.unlink(missing_ok=True)
        self._remove_pid()


def _daemonize() -> None:
    if os.fork() > 0:
        raise SystemExit(0)
    os.setsid()
    if os.fork() > 0:
        raise SystemExit(0)
    sys.stdout.flush()
    sys.stderr.flush()
    with open("/dev/null", "rb", buffering=0) as devnull_in:
        os.dup2(devnull_in.fileno(), sys.stdin.fileno())
    log_path = Path.home() / ".replay-buffer" / "replayd.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_file = log_path.open("a", encoding="utf-8")
    os.dup2(log_file.fileno(), sys.stdout.fileno())
    os.dup2(log_file.fileno(), sys.stderr.fileno())


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Android replay buffer daemon")
    parser.add_argument("--config", type=Path, help="Path to config.yaml")
    parser.add_argument(
        "--foreground",
        action="store_true",
        help="Run in foreground (default for development)",
    )
    parser.add_argument(
        "--daemon",
        action="store_true",
        help="Run as background daemon",
    )
    args = parser.parse_args(argv)

    config = load_config(args.config)
    if args.daemon and not args.foreground:
        _daemonize()

    daemon = ReplayDaemon(config)
    return daemon.run_foreground()


def console_main() -> None:
    raise SystemExit(main())


if __name__ == "__main__":
    console_main()
