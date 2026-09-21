# Real stdio process, Unicode, tool errors and session recovery; no Python SDK.
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
& cargo test --locked --test mcp -- --test-threads=1
if ($LASTEXITCODE -ne 0) { throw 'MCP process regression failed.' }
# Installed real-model OCR is covered by scripts/test-installer.ps1.
