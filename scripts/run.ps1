param([switch]$Legacy)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $projectRoot
if ($Legacy) {
    & python (Join-Path $projectRoot 'SightOCR.py')
    exit $LASTEXITCODE
}
foreach ($relative in @('dist/SightOCR/SightOCR.exe', 'target/release/SightOCR.exe')) {
    $application = Join-Path $projectRoot $relative
    if (Test-Path -LiteralPath $application) {
        Start-Process -FilePath $application -WorkingDirectory $projectRoot
        exit 0
    }
}
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
$cargoPath = if ($cargoCommand) { $cargoCommand.Source } else { Join-Path $env:USERPROFILE '.cargo/bin/cargo.exe' }
if (-not (Test-Path -LiteralPath $cargoPath)) { throw 'Rust is not installed. Install rustup and Visual Studio C++ Build Tools first.' }
& $cargoPath run --release --locked --bin SightOCR
exit $LASTEXITCODE
