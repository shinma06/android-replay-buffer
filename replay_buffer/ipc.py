"""Unix socket IPC between replay CLI and replayd."""

from __future__ import annotations

import json
import socket
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class IpcResponse:
    ok: bool
    message: str
    data: dict[str, Any]

    @classmethod
    def from_dict(cls, payload: dict[str, Any]) -> "IpcResponse":
        return cls(
            ok=bool(payload.get("ok", False)),
            message=str(payload.get("message", "")),
            data=dict(payload.get("data", {})),
        )

    def to_dict(self) -> dict[str, Any]:
        return {"ok": self.ok, "message": self.message, "data": self.data}


def send_command(
    socket_path: Path, command: str, *, timeout: float = 30.0, **params: Any
) -> IpcResponse:
    payload = {"command": command, **params}
    request = json.dumps(payload).encode("utf-8")

    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(timeout)
        client.connect(str(socket_path))
        client.sendall(request)
        chunks: list[bytes] = []
        while True:
            chunk = client.recv(65536)
            if not chunk:
                break
            chunks.append(chunk)

    if not chunks:
        return IpcResponse(ok=False, message="Empty response from replayd", data={})

    try:
        response_payload = json.loads(b"".join(chunks).decode("utf-8"))
    except json.JSONDecodeError as exc:
        return IpcResponse(ok=False, message=f"Invalid JSON response: {exc}", data={})

    return IpcResponse.from_dict(response_payload)


def handle_client(connection: socket.socket, handler) -> None:
    connection.settimeout(120.0)
    try:
        request = connection.recv(65536)
        if not request:
            response = IpcResponse(ok=False, message="Empty request", data={})
        else:
            try:
                payload = json.loads(request.decode("utf-8"))
                response = handler(payload)
            except json.JSONDecodeError as exc:
                response = IpcResponse(
                    ok=False, message=f"Invalid JSON request: {exc}", data={}
                )
            except Exception as exc:  # noqa: BLE001 - return error to CLI
                response = IpcResponse(ok=False, message=str(exc), data={})

        connection.sendall(json.dumps(response.to_dict()).encode("utf-8"))
    finally:
        connection.close()
