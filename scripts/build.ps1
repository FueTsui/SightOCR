param([switch]$Installer, [switch]$SkipOptionalFontTests)
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
# Native font/focus fixtures share Windows desktop state; run them serially.
$testArguments = @('test', '--locked', '--', '--test-threads=1')
if ($SkipOptionalFontTests) {
    # Keep the full default gate. This opt-in excludes only the two tests
    # requiring supplemental Windows Indic font coverage, never input tests.
    Write-Warning 'Skipping two supplemental-font environment tests; record this limit in validation evidence.'
    $testArguments += @(
        '--skip', 'app::fonts::tests::installed_windows_fonts_cover_additional_international_scripts',
        '--skip', 'app::fonts::tests::installed_windows_fonts_cover_every_configured_language')
}
Invoke-Cargo -CargoArguments $testArguments
Invoke-Cargo -CargoArguments @('build', '--release', '--locked', '--bins')
$packageRoot = Join-Path $projectRoot 'dist/SightOCR'
$stagingRoot = Join-Path $projectRoot ('target/package-' + [guid]::NewGuid().ToString('N'))
$modelRoot = Join-Path $stagingRoot 'resources/oneocr'
New-Item -ItemType Directory -Path $modelRoot -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $projectRoot 'target/release/SightOCR.exe') -Destination $stagingRoot
Copy-Item -LiteralPath (Join-Path $projectRoot 'target/release/sightocr-mcp.exe') -Destination $stagingRoot
foreach ($name in @('oneocr.dll', 'onnxruntime.dll', 'oneocr.onemodel')) {
    Copy-Item -LiteralPath (Join-Path $projectRoot "resources/oneocr/$name") -Destination $modelRoot -Force
}
Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $stagingRoot
Copy-Item -LiteralPath (Join-Path $projectRoot 'skills') -Destination $stagingRoot -Recurse
# Ship user documentation only; development records stay in the repository.
$shippedDocs = @('USAGE.md', 'INSTALLATION.md', 'MCP.md')
$stagingDocs = Join-Path $stagingRoot 'docs'
New-Item -ItemType Directory -Path $stagingDocs -Force | Out-Null
foreach ($name in $shippedDocs) {
    Copy-Item -LiteralPath (Join-Path $projectRoot "docs/$name") -Destination $stagingDocs
}
# Drop README lines that link to documents the package does not contain.
$readme = [IO.File]::ReadAllLines((Join-Path $projectRoot 'README.md'), [Text.UTF8Encoding]::new($false)) | Where-Object {
    $links = [regex]::Matches($_, '\]\(docs/([^)#]+\.md)')
    -not ($links | Where-Object { $shippedDocs -notcontains $_.Groups[1].Value })
}
[IO.File]::WriteAllLines((Join-Path $stagingRoot 'README.md'), [string[]]$readme, [Text.UTF8Encoding]::new($false))
# Preserve the local images referenced by the shipped README in both packages.
$imagePrefix = [IO.Path]::GetFullPath($projectRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
$readmeImages = [regex]::Matches(($readme -join "`n"), '<img\s+[^>]*src="([^"]+)"')
foreach ($imageMatch in $readmeImages) {
    $relativeImage = $imageMatch.Groups[1].Value
    $sourceImage = [IO.Path]::GetFullPath((Join-Path $projectRoot $relativeImage))
    if (-not $sourceImage.StartsWith($imagePrefix, [StringComparison]::OrdinalIgnoreCase) -or
        $relativeImage -notmatch '^(assets/|docs/screenshots/)' -or $relativeImage -match '(^|/)\.\.(/|$)') {
        throw "README image is outside the supported package folders: $relativeImage"
    }
    $destinationImage = Join-Path $stagingRoot $relativeImage
    New-Item -ItemType Directory -Path (Split-Path -Parent $destinationImage) -Force | Out-Null
    Copy-Item -LiteralPath $sourceImage -Destination $destinationImage
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
    # SightOCR's own setup program packs the published folder after itself.
    $versionMatch = [regex]::Match((Get-Content -Raw (Join-Path $projectRoot 'Cargo.toml')), '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $versionMatch.Success) { throw 'Package version is missing from Cargo.toml.' }
    $installerDir = Join-Path $projectRoot 'dist/installer'
    New-Item -ItemType Directory -Path $installerDir -Force | Out-Null
    $installerPath = Join-Path $installerDir "SightOCR-Setup-$($versionMatch.Groups[1].Value).exe"
    # The release build has no console, so wait for it explicitly.
    $packer = Start-Process -FilePath (Join-Path $projectRoot 'target/release/SightOCR-Setup.exe') `
        -ArgumentList @('--pack', "`"$packageRoot`"", "`"$installerPath`"") -Wait -PassThru -NoNewWindow
    if ($packer.ExitCode -ne 0) { throw "Installer packaging failed ($($packer.ExitCode))." }
    if (-not (Test-Path -LiteralPath $installerPath -PathType Leaf)) { throw 'Installer output is missing.' }
    $installerHash = (Get-FileHash -LiteralPath $installerPath -Algorithm SHA256).Hash
    $programHash = (Get-FileHash -LiteralPath (Join-Path $packageRoot 'SightOCR.exe') -Algorithm SHA256).Hash
    $mcpHash = (Get-FileHash -LiteralPath (Join-Path $packageRoot 'sightocr-mcp.exe') -Algorithm SHA256).Hash
    # Published next to the setup asset for clients whose GitHub digest is absent.
    $checksumText = "$installerHash  $([IO.Path]::GetFileName($installerPath))`n$programHash  ../SightOCR/SightOCR.exe`n$mcpHash  ../SightOCR/sightocr-mcp.exe`n"
    [IO.File]::WriteAllText((Join-Path $installerDir 'SHA256SUMS.txt'), $checksumText, [Text.UTF8Encoding]::new($false))
    Write-Host "Installer: $installerPath"
    Write-Host "Installer SHA256: $installerHash"
}
Write-Host "Built: $packageRoot/SightOCR.exe"
Write-Host "MCP: $packageRoot/sightocr-mcp.exe"
