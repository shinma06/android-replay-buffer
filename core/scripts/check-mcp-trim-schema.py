#!/usr/bin/env python3
"""Check the live stdio catalog and real trim results with python-jsonschema (Draft 2020-12)."""
import argparse
import json
import runpy
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry

VERSIONS = runpy.run_path(str(Path(__file__).with_name("mcp_versions.py")))["VERSIONS"]


def check_refs(node, root):
    if isinstance(node, dict):
        if "$ref" in node:
            assert node["$ref"].startswith("#/"), node["$ref"]
            target = root
            for key in node["$ref"][2:].split("/"):
                target = target[key.replace("~1", "/").replace("~0", "~")]
        for child in node.values():
            check_refs(child, root)
    elif isinstance(node, list):
        for child in node:
            check_refs(child, root)


def check_version(cli, flags, fixture, version):
    with subprocess.Popen(
        [cli, *flags, "mcp"], text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE
    ) as proc:
        seq = 0

        def call(method, params):
            nonlocal seq
            seq += 1
            request = {"jsonrpc": "2.0", "id": seq, "method": method, "params": params}
            proc.stdin.write(json.dumps(request) + "\n")
            proc.stdin.flush()
            for line in proc.stdout:
                response = json.loads(line)
                if response.get("id") == seq:
                    assert "error" not in response, response
                    return response["result"], len(line.encode("utf-8"))
            raise RuntimeError(f"stdio ended during {method}")

        try:
            hello, _ = call("initialize", {"protocolVersion": version, "capabilities": {},
                                           "clientInfo": {"name": "schema-check", "version": "1"}})
            assert hello["protocolVersion"] == version
            result, size = call("tools/list", {})
            assert size < 90_000, size
            tools = result["tools"]
            assert len(tools) == 20 and "nextCursor" not in result
            for tool in tools:
                for key in ("inputSchema", "outputSchema"):
                    schema = tool[key]
                    check_refs(schema, schema)
                    Draft202012Validator.check_schema(schema)
            trim = next(t for t in tools if t["name"] == "arb_export_trim")
            inputs = Draft202012Validator(trim["inputSchema"], registry=Registry())
            outputs = Draft202012Validator(trim["outputSchema"], registry=Registry())
            for format_name in ("bundle", "share_mp4"):
                args = {"bundle_id": str(fixture), "format": format_name,
                        "in_time_ns": 5_000_001, "out_time_ns": 20_000_007}
                inputs.validate(args)
                exported, _ = call("tools/call", {"name": trim["name"], "arguments": args})
                assert exported["isError"] is False, exported
                body = json.loads(exported["content"][0]["text"])
                if version >= VERSIONS["V2025_06_18"]:
                    assert exported["structuredContent"] == body
                outputs.validate(body)
                for key in ("in_time_ns", "out_time_ns"):
                    assert body[key] == args[key]
            assert not inputs.is_valid({"bundle_id": str(fixture), "in_time_ns": 1})
            print(f"{version}: 20 tools, 40 schemas, 2 real exports, {size} bytes")
        finally:
            proc.stdin.close()


def main():
    root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=root / "core/target/debug")
    args = parser.parse_args()
    cli = args.bin_dir.resolve() / "arb-cli"
    source = next((root / "fixtures/legacy-schema4/replay-trim-parity").glob("replay-*"))
    with tempfile.TemporaryDirectory() as directory:
        tmp = Path(directory)
        # The read-only parity fixture deliberately lacks an epoch origin. Give the
        # synthetic test copy a device clock so all four streams can be selected.
        fixture = tmp / "synthetic"
        shutil.copytree(source, fixture)
        manifest = fixture / "replay.json"
        replay = json.loads(manifest.read_text())
        replay["timebase"]["window_start_elapsed_ns"] = "1000000000"
        manifest.write_text(json.dumps(replay))
        flags = ["--data-dir", tmp / "data", "--cache-dir", tmp / "cache", "--runtime-dir", tmp / "run"]
        with subprocess.Popen([cli.with_name("arbd"), *flags, "--exit-on-stdin-eof"],
                              stdin=subprocess.PIPE) as daemon:
            try:
                deadline = time.monotonic() + 15
                while subprocess.run([cli, *flags, "status"], capture_output=True).returncode:
                    if daemon.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError("arbd did not become ready")
                    time.sleep(0.05)
                assert len(VERSIONS) == 4
                for version in sorted(VERSIONS.values()):
                    check_version(cli, flags, fixture, version)
            finally:
                daemon.stdin.close()


if __name__ == "__main__":
    main()
