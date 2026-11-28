#!/usr/bin/env python3
"""Check the live stdio catalog and real trim results with python-jsonschema (Draft 2020-12).

`tools/list` carries each tool's `inputSchema` only (at most 30,000 bytes per version). The complete
`outputSchema` of every tool is read from `resources/read arb://schemas/{tool}` and checked too.
"""
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
TOOLS_LIST_BUDGET = 30_000


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


def check_silent_success(call, fixture, outputs, rows):
    for row in rows:
        name = row["tool"]
        args = dict(row["arguments"])
        if name != "arb_screenshot":
            args["bundle_id"] = str(fixture)
        response, _ = call("tools/call", {"name": name, "arguments": args})
        body = json.loads(response["content"][0]["text"])
        Draft202012Validator(outputs[name], registry=Registry()).validate(body)
        assert response["isError"] == ("error" in row), (row["id"], body)
        if "error" in row:
            assert (body["error_code"], body["details"]) == (row["error"], row["details"]), body
            assert body["hint"], body
        elif "warning" in row:
            warning, = body["warnings"]
            assert warning["code"] == row["warning"] and warning["message"], body
            if warning["code"] == "contact_sheet_sampled":
                assert body["dropped_frame_indices"] == [1, 2, 3]
                assert warning["details"] == dict(requested=5, rendered=2, max_tiles=2, dropped_count=3)
            else:
                assert body["burned_tap_count"] == warning["details"]["burned_tap_count"] == 0
                assert "physical" in warning["details"]["input_capture"]
        if "info" in row or ("format" in args and "error" not in row):
            path = Path(body["output_path"])
            assert path.exists() and row.get("info", "default_export_dir") in body["info_codes"]
            assert body["resource_links"][0]["uri"].rstrip("/") == path.as_uri().rstrip("/")


def check_version(cli, flags, fixture, version, rows):
    with subprocess.Popen(
        [cli, *flags, "mcp"], text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE
    ) as proc:
        seq = 0

        def request(method, params):
            nonlocal seq
            seq += 1
            message = {"jsonrpc": "2.0", "id": seq, "method": method, "params": params}
            proc.stdin.write(json.dumps(message) + "\n")
            proc.stdin.flush()
            for line in proc.stdout:
                response = json.loads(line)
                if response.get("id") == seq:
                    return response, len(line.encode("utf-8"))
            raise RuntimeError(f"stdio ended during {method}")

        def call(method, params):
            response, size = request(method, params)
            assert "error" not in response, response
            return response["result"], size

        try:
            hello, _ = call("initialize", {"protocolVersion": version, "capabilities": {},
                                           "clientInfo": {"name": "schema-check", "version": "1"}})
            assert hello["protocolVersion"] == version
            result, size = call("tools/list", {})
            assert size <= TOOLS_LIST_BUDGET, size
            tools = result["tools"]
            assert len(tools) == 21 and "nextCursor" not in result
            outputs_by_tool = {}
            for tool in tools:
                assert "outputSchema" not in tool, tool["name"]
                uri = f"arb://schemas/{tool['name']}"
                read, _ = call("resources/read", {"uri": uri})
                content = read["contents"][0]
                assert content["uri"] == uri and content["mimeType"] == "application/schema+json"
                output = json.loads(content["text"])
                assert output["$schema"] == "https://json-schema.org/draft/2020-12/schema"
                outputs_by_tool[tool["name"]] = output
                for schema in (tool["inputSchema"], output):
                    check_refs(schema, schema)
                    Draft202012Validator.check_schema(schema)
            for uri in ("arb://schemas/arb_unknown", "arb://schemas/../arb_status",
                        "arb://schemas/%2e%2e", "arb://schemas/"):
                response, _ = request("resources/read", {"uri": uri})
                assert response["error"]["code"] == -32602, response
            check_silent_success(call, fixture, outputs_by_tool, rows)
            trim = next(t for t in tools if t["name"] == "arb_export_trim")
            inputs = Draft202012Validator(trim["inputSchema"], registry=Registry())
            outputs = Draft202012Validator(outputs_by_tool[trim["name"]], registry=Registry())
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
            print(f"{version}: 21 tools, 21 inputSchema + 21 outputSchema (arb://schemas), "
                  f"2 real exports, tools/list {size} bytes")
        finally:
            proc.stdin.close()


def main():
    root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=root / "core/target/debug")
    args = parser.parse_args()
    cli = args.bin_dir.resolve() / "arb-cli"
    source = next((root / "fixtures/legacy-schema4/replay-trim-parity").glob("replay-*"))
    table = json.loads((root / "docs/specs/mcp-tools-table.json").read_text(encoding="utf-8"))
    warning = Draft202012Validator(table["$defs"]["WarningDetail"])
    sample = {"code": "test", "message": "例"}
    warning.validate(sample)
    warning.validate({**sample, "details": {"count": 0}})
    assert not warning.is_valid({**sample, "details": []})
    rows = json.loads((root / "docs/specs/ui-feedback-contract-table.json").read_text())["silent_success_rows"]
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
                    check_version(cli, flags, fixture, version, rows)
            finally:
                daemon.stdin.close()


if __name__ == "__main__":
    main()
