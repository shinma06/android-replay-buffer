"""replay CLI — control replayd from the terminal."""

from __future__ import annotations

import argparse
import os
import signal
import socket
import sys
from pathlib import Path

from replay_buffer.config import load_config
from replay_buffer.daemon import main as daemon_main
from replay_buffer.ipc import send_command


def _ensure_daemon_running(config_path: Path | None) -> None:
    config = load_config(config_path)
    if config.socket_path.exists():
        return

    if config.pid_path.exists():
        pid_text = config.pid_path.read_text(encoding="utf-8").strip()
        if pid_text.isdigit():
            pid = int(pid_text)
            try:
                os.kill(pid, 0)
            except OSError:
                config.pid_path.unlink(missing_ok=True)
            else:
                raise RuntimeError(
                    f"replayd appears to be starting (pid {pid}) but socket is missing"
                )

    argv = [sys.executable, "-m", "replay_buffer.daemon", "--daemon"]
    if config_path is not None:
        argv.extend(["--config", str(config_path)])

    import subprocess

    package_root = Path(__file__).resolve().parent.parent
    env = os.environ.copy()
    existing_pythonpath = env.get("PYTHONPATH", "")
    env["PYTHONPATH"] = (
        str(package_root)
        if not existing_pythonpath
        else f"{package_root}{os.pathsep}{existing_pythonpath}"
    )

    subprocess.Popen(
        argv,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
        cwd=str(package_root),
        env=env,
    )

    for _ in range(30):
        if config.socket_path.exists():
            return
        import time

        time.sleep(0.2)
    raise RuntimeError("replayd failed to start")


def cmd_save(config_path: Path | None) -> int:
    _ensure_daemon_running(config_path)
    config = load_config(config_path)
    try:
        response = send_command(config.socket_path, "save", timeout=120.0)
    except (OSError, socket.timeout):
        print(
            "replayd is not responding. Restart it with: replayd",
            file=sys.stderr,
        )
        return 1
    if not response.ok:
        print(response.message, file=sys.stderr)
        return 1
    path = response.data.get("path", "")
    print(path or response.message)
    return 0


def cmd_status(config_path: Path | None) -> int:
    config = load_config(config_path)
    if not config.socket_path.exists():
        print("replayd: not running")
        return 1
    if config.pid_path.exists():
        pid_text = config.pid_path.read_text(encoding="utf-8").strip()
        if pid_text.isdigit():
            try:
                os.kill(int(pid_text), 0)
            except OSError:
                print("replayd: not running")
                return 1

    response = send_command(config.socket_path, "status", timeout=10.0)
    if not response.ok:
        print(response.message, file=sys.stderr)
        return 1

    data = response.data
    state = data.get("state", "unknown")
    if state == "waiting":
        print("Waiting for Android device...")
        print(f"Replay: {data.get('replay_seconds')} sec")
        return 0

    device_name = data.get("device_name", "device")
    replay_seconds = data.get("replay_seconds")
    if data.get("recording"):
        print(f"● {device_name} / {replay_seconds}s")
    else:
        print(f"{device_name}: not recording")

    error = data.get("error")
    if error:
        print(f"Error: {error}", file=sys.stderr)
        return 1
    return 0


def cmd_stop(config_path: Path | None) -> int:
    config = load_config(config_path)
    if not config.socket_path.exists():
        print("replayd: not running")
        return 0

    response = send_command(config.socket_path, "stop")
    if not response.ok:
        print(response.message, file=sys.stderr)
        return 1

    if config.pid_path.exists():
        pid_text = config.pid_path.read_text(encoding="utf-8").strip()
        if pid_text.isdigit():
            try:
                os.kill(int(pid_text), signal.SIGTERM)
            except OSError:
                pass
    print("replayd stopped")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="replay", description="Android replay buffer CLI")
    parser.add_argument("--config", type=Path, help="Path to config.json")
    subparsers = parser.add_subparsers(dest="command", required=True)

    subparsers.add_parser("save", help="Save the last N seconds")
    subparsers.add_parser("status", help="Show daemon status")
    subparsers.add_parser("stop", help="Stop replayd")

    daemon_parser = subparsers.add_parser("daemon", help="Run replayd in foreground")
    daemon_parser.add_argument(
        "--foreground",
        action="store_true",
        help="Run replayd in foreground",
    )

    args = parser.parse_args(argv)
    if args.command == "save":
        return cmd_save(args.config)
    if args.command == "status":
        return cmd_status(args.config)
    if args.command == "stop":
        return cmd_stop(args.config)
    if args.command == "daemon":
        daemon_argv = ["--foreground"]
        if args.config is not None:
            daemon_argv.extend(["--config", str(args.config)])
        return daemon_main(daemon_argv)
    return 1


def console_main() -> None:
    raise SystemExit(main())


if __name__ == "__main__":
    console_main()
