"""Release smoke test with the official MCP Python client.

Developer dependencies only: python -m pip install mcp jsonschema
All inputs are synthetic; this script never invokes an online translator.
"""

import argparse
import asyncio
from datetime import timedelta
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import subprocess
import tempfile

from jsonschema import Draft202012Validator
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run_cli(binary, arguments, env, text=None, success=True):
    result = subprocess.run(
        [str(binary), *arguments], input=None if text is None else text.encode("utf-8"), capture_output=True,
        env=env, timeout=45, creationflags=subprocess.CREATE_NO_WINDOW,
    )
    assert (result.returncode == 0) == success, (arguments, result.returncode, result.stderr)
    if success:
        assert not result.stderr, result.stderr
        return json.loads(result.stdout)
    assert not result.stdout, result.stdout
    assert result.stderr, "Missing error diagnostics"


async def test_mcp(binary, env, fixture, work):
    params = StdioServerParameters(command=str(binary), args=["mcp"], env=env, cwd=str(work))
    with (work / "mcp-stderr.log").open("w", encoding="utf-8") as errlog:
        async with stdio_client(params, errlog=errlog) as (reader, writer):
            async with ClientSession(reader, writer, read_timeout_seconds=timedelta(seconds=45)) as session:
                initialized = await session.initialize()
                listing = await session.list_tools()
                tools = {tool.name: tool for tool in listing.tools}
                assert set(tools) == {"sightocr_ocr", "sightocr_translate", "sightocr_languages"}
                results = {}
                calls = [
                    ("sightocr_languages", {}),
                    ("sightocr_translate", {"text": "你好，世界\nHello world", "source_lang": "zh-Hans", "target_lang": "zh-Hans", "provider": "bing"}),
                    ("sightocr_ocr", {"image_path": str(fixture), "table": True}),
                ]
                for name, arguments in calls:
                    Draft202012Validator(tools[name].inputSchema).validate(arguments)
                    result = await session.call_tool(name, arguments)
                    assert not result.isError, result
                    assert result.structuredContent is not None, result
                    assert tools[name].outputSchema is not None
                    Draft202012Validator(tools[name].outputSchema).validate(result.structuredContent)
                    assert any(item.type == "text" for item in result.content)
                    results[name] = result.structuredContent
                assert results["sightocr_translate"]["text"] == "你好，世界\nHello world"
                assert "Alpha\t100" in results["sightocr_ocr"]["text"]
                assert "Beta\t200" in results["sightocr_ocr"]["text"]
                # Tool execution failure must remain recoverable in this session.
                failure = await session.call_tool("sightocr_ocr", {"image_path": str(work / "missing.png")})
                assert failure.isError
                await session.send_ping()
                return {"protocol": initialized.protocolVersion, "tools": sorted(tools), "results": results, "recoverable_error": True}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--resources", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    binary = args.binary.resolve(strict=True)
    fixture = (root / "tests/fixtures/basic.png").resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="sightocr-cli-mcp-") as directory:
        work = Path(directory)
        config = work / "config.json"
        config.write_text('{"source_lang":"en","target_lang":"en","sentinel":"preserve"}', encoding="utf-8")
        before = digest(config)
        env = dict(os.environ, SIGHTOCR_CONFIG=str(config))
        if args.resources:
            env["SIGHTOCR_RESOURCES"] = str(args.resources.resolve(strict=True))
        else:
            env.pop("SIGHTOCR_RESOURCES", None)
        languages = run_cli(binary, ["languages", "--format", "json"], env)
        assert any(item["code"] == "zh-Hans" for item in languages["languages"])
        ocr = run_cli(binary, ["ocr", str(fixture), "--table", "--format", "json"], env)
        assert "Alpha\t100" in ocr["text"] and "Beta\t200" in ocr["text"]
        text = "你好，世界\nHello world\n"
        translation = run_cli(binary, ["translate", "--stdin", "--from", "zh-Hans", "--to", "zh-Hans", "--format", "json"], env, text)
        assert translation["text"] == text, (repr(translation["text"]), repr(text))
        source = work / "输入 文件.txt"
        destination = work / "输出 文件.json"
        source.write_text("\ufeff" + text, encoding="utf-8", newline="")
        process = subprocess.run([str(binary), "translate", "--input", str(source), "--from", "en", "--to", "en", "--format", "json", "--output", str(destination)], env=env, capture_output=True, timeout=45, creationflags=subprocess.CREATE_NO_WINDOW)
        assert process.returncode == 0 and not process.stdout and not process.stderr
        assert json.loads(destination.read_text(encoding="utf-8"))["text"] == text
        run_cli(binary, ["translate", "hello", "--to", "auto"], env, success=False)
        run_cli(binary, ["ocr", str(work / "missing.png")], env, success=False)
        mcp = asyncio.run(test_mcp(binary, env, fixture, work))
        assert digest(config) == before, "CLI/MCP modified the configuration"
        absent = work / "never-created.json"
        absent_env = dict(env, SIGHTOCR_CONFIG=str(absent))
        run_cli(binary, ["translate", "unchanged", "--from", "en", "--to", "en", "--format", "json"], absent_env)
        assert not absent.exists(), "CLI created a missing config"
        report = {
            "complete": True, "binary": str(binary), "sha256": digest(binary),
            "mcp_sdk": importlib.metadata.version("mcp"), "cli_utf8_stdin_file_json": True,
            "cli_local_ocr": ocr, "configuration_preserved": True,
            "missing_configuration_not_created": True, "online_translation": "Not called; same-language translation only",
            "mcp": mcp,
        }
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(json.dumps({"complete": True, "report": str(args.report.resolve()), "mcp_protocol": mcp["protocol"]}))


if __name__ == "__main__":
    main()
