param([switch]$Installer, [string]$IsccPath = $env:SIGHTOCR_ISCC)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $projectRoot
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
$cargoPath = if ($cargoCommand) { $cargoCommand.Source } else { Join-Path $env:USERPROFILE '.cargo/bin/cargo.exe' }
if (-not (Test-Path -LiteralPath $cargoPath)) { throw 'Rust toolchain not found.' }
function Invoke-Cargo([string[]]$CargoArguments) {
    & $cargoPath @CargoArguments
    if ($LASTEXITCODE -ne 0) { throw "cargo $CargoArguments failed ($LASTEXITCODE)." }
}
Invoke-Cargo -CargoArguments @('fmt', '--all', '--', '--check')
Invoke-Cargo -CargoArguments @('clippy', '--all-targets', '--locked', '--', '-D', 'warnings')
Invoke-Cargo -CargoArguments @('test', '--locked')
Invoke-Cargo -CargoArguments @('build', '--release', '--locked', '--bin', 'SightOCR')
$packageRoot = Join-Path $projectRoot 'dist/SightOCR'
$stagingRoot = Join-Path $projectRoot ('target/package-' + [guid]::NewGuid().ToString('N'))
$modelRoot = Join-Path $stagingRoot 'resources/oneocr'
New-Item -ItemType Directory -Path $modelRoot -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $projectRoot 'target/release/SightOCR.exe') -Destination $stagingRoot
foreach ($name in @('oneocr.dll', 'onnxruntime.dll', 'oneocr.onemodel')) {
    Copy-Item -LiteralPath (Join-Path $projectRoot "resources/oneocr/$name") -Destination $modelRoot -Force
}
Copy-Item -LiteralPath (Join-Path $projectRoot 'README.md') -Destination $stagingRoot
Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $stagingRoot
$stagingDocs = Join-Path $stagingRoot 'docs'
New-Item -ItemType Directory -Path $stagingDocs -Force | Out-Null
foreach ($name in @('RUST_REFACTOR.md', 'VALIDATION.md', 'UI_DESIGN.md', 'INSTALLATION.md')) {
    $sourceDocument = Join-Path $projectRoot "docs/$name"
    if (Test-Path -LiteralPath $sourceDocument) { Copy-Item -LiteralPath $sourceDocument -Destination $stagingDocs }
}
# Preserve existing packages outside the publish directory rather than carrying
# stale Python bundles, user configuration or logs into the new package.
function Assert-WorkspacePath([string]$Candidate) {
    $workspacePrefix = [IO.Path]::GetFullPath($projectRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $resolvedCandidate = [IO.Path]::GetFullPath($Candidate)
    if (-not $resolvedCandidate.StartsWith($workspacePrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing filesystem operation outside workspace: $resolvedCandidate"
    }
}
$previousPackage = $null
New-Item -ItemType Directory -Path (Split-Path -Parent $packageRoot) -Force | Out-Null
if (Test-Path -LiteralPath $packageRoot) {
    $previousPackage = Join-Path $projectRoot ('target/package-backups/SightOCR-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path (Split-Path -Parent $previousPackage) -Force | Out-Null
    Assert-WorkspacePath $packageRoot
    Assert-WorkspacePath $previousPackage
    Move-Item -LiteralPath $packageRoot -Destination $previousPackage
}
try {
    Assert-WorkspacePath $stagingRoot
    Assert-WorkspacePath $packageRoot
    Move-Item -LiteralPath $stagingRoot -Destination $packageRoot
} catch {
    if ($previousPackage -and -not (Test-Path -LiteralPath $packageRoot)) {
        Assert-WorkspacePath $previousPackage
        Assert-WorkspacePath $packageRoot
        Move-Item -LiteralPath $previousPackage -Destination $packageRoot
    }
    throw
}
if ($Installer) {
    $compiler = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    $candidates = @($IsccPath)
    if ($compiler) { $candidates += $compiler.Source }
    $candidates += Join-Path $projectRoot 'target/tools/innosetup-6.7.3/ISCC.exe'
    if (${env:ProgramFiles(x86)}) { $candidates += Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6/ISCC.exe' }
    $resolvedCompiler = $candidates | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } | Select-Object -First 1
    if (-not $resolvedCompiler) { throw 'Inno Setup 6 is required. Run scripts/prepare-installer.ps1, or set SIGHTOCR_ISCC / -IsccPath.' }
    if ($IsccPath -and -not (Test-Path -LiteralPath $IsccPath -PathType Leaf)) { throw 'The explicit Inno compiler path does not exist.' }
    $versionMatch = [regex]::Match((Get-Content -Raw (Join-Path $projectRoot 'Cargo.toml')), '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $versionMatch.Success) { throw 'Package version is missing from Cargo.toml.' }
    & $resolvedCompiler "/DMyAppVersion=$($versionMatch.Groups[1].Value)" (Join-Path $projectRoot 'SightOCR.iss')
    if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed.' }
    $installerPath = Join-Path $projectRoot "dist/installer/SightOCR-Setup-$($versionMatch.Groups[1].Value).exe"
    if (-not (Test-Path -LiteralPath $installerPath -PathType Leaf)) { throw 'Installer output is missing.' }
    $installerHash = (Get-FileHash -LiteralPath $installerPath -Algorithm SHA256).Hash
    $programHash = (Get-FileHash -LiteralPath (Join-Path $packageRoot 'SightOCR.exe') -Algorithm SHA256).Hash
    # Publish alongside the setup asset for clients whose GitHub digest is absent.
    $checksumText = "$installerHash  $([IO.Path]::GetFileName($installerPath))`n$programHash  ../SightOCR/SightOCR.exe`n"
    [IO.File]::WriteAllText((Join-Path $projectRoot 'dist/installer/SHA256SUMS.txt'), $checksumText, [Text.UTF8Encoding]::new($false))
    Write-Host "Installer: $installerPath"
    Write-Host "Installer SHA256: $installerHash"
}
Write-Host "Built: $packageRoot/SightOCR.exe"
