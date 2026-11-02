# Real CLI processes: pipes, Unicode input files, JSON output and exit codes; no network.
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)
& cargo test --locked --test cli -- --test-threads=1
if ($LASTEXITCODE -ne 0) { throw 'CLI process regression failed.' }
# Installed real-model OCR is covered by scripts/test-installer.ps1.
