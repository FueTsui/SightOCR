param([string]$IsccPath = $env:SIGHTOCR_ISCC, [switch]$ShutdownOnly, [switch]$VerifyUpdateTimeout)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
if (-not $IsccPath) { $IsccPath = Join-Path $projectRoot 'target/tools/innosetup-6.7.3/ISCC.exe' }
if (-not (Test-Path -LiteralPath $IsccPath -PathType Leaf)) { throw 'Prepare Inno Setup or pass -IsccPath.' }
$packageRoot = Join-Path $projectRoot 'dist/SightOCR'
$testRoot = Join-Path $projectRoot ('target/installer-smoke-' + [guid]::NewGuid().ToString('N'))
$installRoot = Join-Path $testRoot 'installed'
$versionMatch = [regex]::Match((Get-Content -Raw (Join-Path $projectRoot 'Cargo.toml')), '(?m)^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) { throw 'Package version is missing.' }
$version = $versionMatch.Groups[1].Value
New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
function Assert-TestPath([string]$Candidate) {
    $prefix = [IO.Path]::GetFullPath($testRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    if (-not [IO.Path]::GetFullPath($Candidate).StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing operation outside isolated test directory: $Candidate"
    }
}
function Run-Hidden([string]$File, [string[]]$Arguments) {
    Assert-TestPath $File
    $process = Start-Process -FilePath $File -ArgumentList $Arguments -WindowStyle Hidden -PassThru
    if (-not $process.WaitForExit(150000)) { throw 'Isolated installer process exceeded 150 seconds; inspect its log.' }
    if ($process.ExitCode -ne 0) { throw "Isolated installer process failed ($($process.ExitCode))." }
}
function Test-InstallerShutdown {
    $fixtureRoot = Join-Path $testRoot 'shutdown'
    $fixturePackage = Join-Path $fixtureRoot 'package'
    $fixtureInstall = Join-Path $fixtureRoot 'installed'
    $otherInstall = Join-Path $fixtureRoot 'other-installation'
    foreach ($directory in @($fixturePackage, $fixtureInstall, $otherInstall, (Join-Path $fixturePackage 'docs'), (Join-Path $fixturePackage 'resources/oneocr'))) {
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
    }
    $fixtureExe = Join-Path $fixturePackage 'SightOCR.exe'
    $csc = Join-Path $env:windir 'Microsoft.NET/Framework64/v4.0.30319/csc.exe'
    & $csc /nologo /target:winexe /reference:System.Windows.Forms.dll "/out:$fixtureExe" (Join-Path $PSScriptRoot 'installer/ShutdownFixture.cs')
    if ($LASTEXITCODE -ne 0) { throw 'Synthetic shutdown fixture compilation failed.' }
    Copy-Item -LiteralPath $fixtureExe -Destination (Join-Path $fixturePackage 'sightocr-cli.exe')
    if (-not ('ShutdownFixture' -as [type])) { [Reflection.Assembly]::LoadFrom($fixtureExe) | Out-Null }
    foreach ($relative in @('README.md', 'LICENSE', 'docs/test.md', 'resources/oneocr/oneocr.dll', 'resources/oneocr/onnxruntime.dll', 'resources/oneocr/oneocr.onemodel')) {
        [IO.File]::WriteAllText((Join-Path $fixturePackage $relative), 'Synthetic installer fixture')
    }
    & $IsccPath "/DMyAppVersion=$version" '/DSmokeTest=1' "/DSmokeInstallDir=$fixtureInstall" "/DPackageDir=$fixturePackage" "/O$fixtureRoot" '/FShutdown-Smoke' (Join-Path $projectRoot 'SightOCR.iss')
    if ($LASTEXITCODE -ne 0) { throw 'Shutdown fixture installer compilation failed.' }
    $fixtureInstaller = Join-Path $fixtureRoot 'Shutdown-Smoke.exe'
    $silentArguments = @('/SP-', '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOICONS', '/TASKS=', '/LANG=chinesesimplified', ('/DIR="' + $fixtureInstall + '"'))
    Run-Hidden $fixtureInstaller $silentArguments
    Copy-Item -LiteralPath $fixtureExe -Destination (Join-Path $otherInstall 'SightOCR.exe')
    function Start-Fixture([string]$Directory, [string]$Executable = 'SightOCR.exe') {
        $file = Join-Path $Directory $Executable
        Assert-TestPath $file
        $process = Start-Process -FilePath $file -WindowStyle Hidden -PassThru
        $timer = [Diagnostics.Stopwatch]::StartNew()
        while ($timer.ElapsedMilliseconds -lt 10000) {
            $events = Join-Path $Directory 'events.log'
            if ((Test-Path -LiteralPath $events) -and [IO.File]::ReadAllText($events).Contains("started:$($process.Id)`n")) { return $process }
            if ($process.HasExited) { throw 'Synthetic app exited before creating its window.' }
            Start-Sleep -Milliseconds 50
        }
        throw 'Synthetic app did not create its window.'
    }
    function Stop-Fixture([string]$Directory) {
        Assert-TestPath $Directory
        foreach ($process in (Get-Process SightOCR,sightocr-cli -ErrorAction SilentlyContinue | Where-Object { $_.Path -in @((Join-Path $Directory 'SightOCR.exe'), (Join-Path $Directory 'sightocr-cli.exe')) })) {
            [ShutdownFixture]::RequestShutdown($process.Id)
            if (-not $process.WaitForExit(5000)) { $process.Kill(); $process.WaitForExit() }
        }
    }
    function Run-WithDialogResponse([int]$Button) {
        $arguments = @($silentArguments | Where-Object { $_ -ne '/SUPPRESSMSGBOXES' })
        $setup = Start-Process -FilePath $fixtureInstaller -ArgumentList $arguments -WindowStyle Hidden -PassThru
        $timer = [Diagnostics.Stopwatch]::StartNew()
        $responded = $false
        while (-not $setup.HasExited -and $timer.ElapsedMilliseconds -lt 20000) {
            $processIds = @($setup.Id)
            $processIds += Get-CimInstance Win32_Process -Filter "ParentProcessId=$($setup.Id)" | ForEach-Object { $_.ProcessId }
            foreach ($processId in $processIds) {
                if ([ShutdownFixture]::RespondToInstaller($processId, $Button)) { $responded = $true; break }
            }
            if ($responded) { break }
            Start-Sleep -Milliseconds 100
        }
        if (-not $responded) { throw 'Isolated installer did not show its automatic-close confirmation.' }
        if (-not $setup.WaitForExit(30000)) { throw 'Isolated installer did not exit after the synthetic response.' }
        return $setup.ExitCode
    }
    $other = $null
    try {
        $other = Start-Fixture $otherInstall
        $running = Start-Fixture $fixtureInstall
        # A suppressed ordinary install must decline shutdown; it has no approval.
        $cancelled = Start-Process -FilePath $fixtureInstaller -ArgumentList $silentArguments -WindowStyle Hidden -PassThru
        if (-not $cancelled.WaitForExit(30000) -or $cancelled.ExitCode -eq 0 -or $running.HasExited) { throw 'Unapproved silent install did not preserve the running app.' }
        if ((Run-WithDialogResponse 2) -eq 0 -or $running.HasExited) { throw 'Cancel did not preserve the running app and exit installation.' }
        if ((Run-WithDialogResponse 1) -ne 0 -or -not $running.WaitForExit(5000)) { throw 'Confirmed installation did not close the running app.' }
        $eventsFile = Join-Path $fixtureInstall 'events.log'
        if (-not [IO.File]::ReadAllText($eventsFile).Contains("graceful:$($running.Id)`n")) { throw 'Confirmed installation did not use graceful exit.' }
        $console = Start-Fixture $fixtureInstall 'sightocr-cli.exe'
        $consoleCancelled = Start-Process -FilePath $fixtureInstaller -ArgumentList $silentArguments -WindowStyle Hidden -PassThru
        if (-not $consoleCancelled.WaitForExit(30000) -or $consoleCancelled.ExitCode -eq 0 -or $console.HasExited) { throw 'Unapproved silent install did not preserve the running CLI/MCP process.' }
        if ((Run-WithDialogResponse 1) -ne 0 -or -not $console.WaitForExit(5000)) { throw 'Confirmed installation did not close the CLI/MCP process.' }
        # The compatibility path applies only to a fixture without the new marker.
        $legacyFlag = Join-Path $fixtureInstall 'legacy'
        [IO.File]::WriteAllText($legacyFlag, '')
        $legacy = Start-Fixture $fixtureInstall
        if ((Run-WithDialogResponse 1) -ne 0 -or -not $legacy.WaitForExit(5000)) { throw 'Confirmed installation did not close the legacy app.' }
        Assert-TestPath $legacyFlag
        Remove-Item -LiteralPath $legacyFlag
        $running = Start-Fixture $fixtureInstall
        $startsBeforeUpdate = @([IO.File]::ReadAllLines($eventsFile) | Where-Object { $_.StartsWith('started:') }).Count
        Run-Hidden $fixtureInstaller ($silentArguments + '/UPDATE' + ('/LOG="' + (Join-Path $fixtureRoot 'update.log') + '"'))
        if (-not $running.WaitForExit(5000) -or -not [IO.File]::ReadAllText($eventsFile).Contains("graceful:$($running.Id)`n")) { throw 'Update did not request and wait for graceful exit.' }
        $timer = [Diagnostics.Stopwatch]::StartNew()
        do {
            Start-Sleep -Milliseconds 50
            $startsAfterUpdate = @([IO.File]::ReadAllLines($eventsFile) | Where-Object { $_.StartsWith('started:') }).Count
        } while ($startsAfterUpdate -le $startsBeforeUpdate -and $timer.ElapsedMilliseconds -lt 10000)
        if ($startsAfterUpdate -ne $startsBeforeUpdate + 1) { throw 'Update did not restart the installed app exactly once.' }
        if ($other.HasExited) { throw 'Installer closed the unrelated installation.' }
        $timeoutResult = 'Not requested; pass -VerifyUpdateTimeout for the 120-second failure case.'
        if ($VerifyUpdateTimeout) {
            Stop-Fixture $fixtureInstall
            [IO.File]::WriteAllText($legacyFlag, '')
            $blocked = Start-Fixture $fixtureInstall
            $protectedFile = Join-Path $fixtureInstall 'README.md'
            [IO.File]::WriteAllText($protectedFile, 'Update must not overwrite this while the app cannot exit.')
            $protectedHash = (Get-FileHash -LiteralPath $protectedFile -Algorithm SHA256).Hash
            $failureArguments = $silentArguments + '/UPDATE' + ('/LOG="' + (Join-Path $fixtureRoot 'update-timeout.log') + '"')
            $blockedSetup = Start-Process -FilePath $fixtureInstaller -ArgumentList $failureArguments -WindowStyle Hidden -PassThru
            if (-not $blockedSetup.WaitForExit(150000) -or $blockedSetup.ExitCode -eq 0) { throw 'Update did not fail when the app could not exit.' }
            if ($blocked.HasExited -or (Get-FileHash -LiteralPath $protectedFile -Algorithm SHA256).Hash -ne $protectedHash) { throw 'Timed-out update terminated the app or replaced files.' }
            $timeoutResult = 'Passed: installer failed without terminating the app or replacing files.'
        }
        [pscustomobject]@{ Complete = $true; CancelPreservesApp = $true; NoUnapprovedSilentShutdown = $true; ConfirmedGracefulExit = $true; ConfirmedLegacyExit = $true; UpdateGracefulExitAndSingleRestart = $true; OtherInstallationPreserved = $true; UpdateTimeout = $timeoutResult } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $fixtureRoot 'report.json') -Encoding UTF8
    } finally {
        Stop-Fixture $fixtureInstall
        Stop-Fixture $otherInstall
    }
    Write-Host "Installer shutdown report: $fixtureRoot/report.json"
}
Test-InstallerShutdown
if ($ShutdownOnly) { return }
$compileArgs = @(
    "/DMyAppVersion=$version", '/DSmokeTest=1', "/DSmokeInstallDir=$installRoot",
    "/DPackageDir=$packageRoot", "/O$testRoot", '/FSightOCR-Smoke',
    (Join-Path $projectRoot 'SightOCR.iss')
)
& $IsccPath @compileArgs
if ($LASTEXITCODE -ne 0) { throw 'Isolated installer compilation failed.' }
$installer = Join-Path $testRoot 'SightOCR-Smoke.exe'
$configSentinel = Join-Path $installRoot 'config.json'
$configText = '{"synthetic_install_test":true}'
[IO.File]::WriteAllText($configSentinel, $configText)
$isolatedConfig = Join-Path $testRoot 'runtime-config.json'
[IO.File]::WriteAllText($isolatedConfig, '{}')
$payload = @('SightOCR.exe', 'sightocr-cli.exe', 'README.md', 'LICENSE', 'resources/oneocr/oneocr.dll', 'resources/oneocr/onnxruntime.dll', 'resources/oneocr/oneocr.onemodel')
$payload += Get-ChildItem -LiteralPath (Join-Path $packageRoot 'docs') -Filter '*.md' | ForEach-Object { 'docs/' + $_.Name }
$hashes = @{}
foreach ($relative in $payload) {
    $hashes[$relative] = (Get-FileHash -LiteralPath (Join-Path $packageRoot $relative) -Algorithm SHA256).Hash
}
$smokeRegistryPath = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\SightOCR.InstallerSmoke_is1'
if (Test-Path -LiteralPath $smokeRegistryPath) { throw 'An unexpected smoke uninstall key already exists; not modifying it.' }
$installArguments = @('/SP-', '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOCLOSEAPPLICATIONS', '/NOICONS', '/TASKS=', '/LANG=chinesesimplified', ('/DIR="' + $installRoot + '"'))
# Repeat installation to verify in-place replacement also preserves unknown files.
for ($attempt = 1; $attempt -le 2; $attempt++) {
    Run-Hidden $installer ($installArguments + ('/LOG="' + (Join-Path $testRoot "install-$attempt.log") + '"'))
    foreach ($relative in $payload) {
        $installed = Join-Path $installRoot $relative
        if ((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -ne $hashes[$relative]) {
            throw "Installed payload mismatch: $relative"
        }
    }
    if ([IO.File]::ReadAllText($configSentinel) -ne $configText) { throw 'Installation changed existing configuration.' }
    if (Test-Path -LiteralPath $smokeRegistryPath) { throw 'Isolated installer created uninstall registration.' }
    $unexpected = Get-Process SightOCR -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $installRoot 'SightOCR.exe') }
    if ($unexpected) { throw 'Silent installation unexpectedly started SightOCR.' }
}
# Exercise the real console host, which has no GUI graceful-exit message.
# Automatic update must fail promptly without closing it or replacing payload.
$consoleStart = [Diagnostics.ProcessStartInfo]::new()
$consoleStart.FileName = Join-Path $installRoot 'sightocr-cli.exe'
$consoleStart.Arguments = 'mcp'
$consoleStart.UseShellExecute = $false
$consoleStart.CreateNoWindow = $true
$consoleStart.RedirectStandardInput = $true
$consoleStart.RedirectStandardOutput = $true
$consoleStart.RedirectStandardError = $true
$consoleStart.EnvironmentVariables['SIGHTOCR_CONFIG'] = $isolatedConfig
Assert-TestPath $consoleStart.FileName
$consoleHost = [Diagnostics.Process]::Start($consoleStart)
try {
    $consoleHost.StandardInput.WriteLine('{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"installer-smoke","version":"1"},"capabilities":{}}}')
    $consoleHost.StandardInput.Flush()
    $ready = $consoleHost.StandardOutput.ReadLineAsync()
    if (-not $ready.Wait(5000) -or -not ($ready.Result | ConvertFrom-Json).result.serverInfo) { throw 'Installed MCP host did not initialize.' }
    $updateLog = Join-Path $testRoot 'update-active-mcp.log'
    Assert-TestPath $installer
    $blockedUpdate = Start-Process -FilePath $installer -ArgumentList ($installArguments + '/UPDATE' + ('/LOG="' + $updateLog + '"')) -WindowStyle Hidden -PassThru
    if (-not $blockedUpdate.WaitForExit(10000)) { throw 'Update waited instead of promptly reporting the active MCP host.' }
    if ($blockedUpdate.ExitCode -eq 0 -or $consoleHost.HasExited) { throw 'Update should preserve the active MCP host and fail before replacing files.' }
    foreach ($relative in $payload) {
        if ((Get-FileHash -LiteralPath (Join-Path $installRoot $relative) -Algorithm SHA256).Hash -ne $hashes[$relative]) { throw 'Blocked update modified the installed payload.' }
    }
} finally {
    $consoleHost.StandardInput.Close()
    if (-not $consoleHost.WaitForExit(5000)) { $consoleHost.Kill(); $consoleHost.WaitForExit() }
    $consoleHost.Dispose()
}
$ocrOutput = Join-Path $testRoot 'ocr.tsv'
$previousConfig = $env:SIGHTOCR_CONFIG
$previousResources = $env:SIGHTOCR_RESOURCES
try {
    $env:SIGHTOCR_CONFIG = $isolatedConfig
    $env:SIGHTOCR_RESOURCES = Join-Path $installRoot 'resources/oneocr'
    Run-Hidden (Join-Path $installRoot 'SightOCR.exe') @('--ocr', ('"' + (Join-Path $projectRoot 'tests/fixtures/basic.png') + '"'), '--table', '--output', ('"' + $ocrOutput + '"'))
    $cliOutput = Join-Path $testRoot 'cli-ocr.json'
    Run-Hidden (Join-Path $installRoot 'sightocr-cli.exe') @('ocr', ('"' + (Join-Path $projectRoot 'tests/fixtures/basic.png') + '"'), '--table', '--format', 'json', '--output', ('"' + $cliOutput + '"'))
} finally {
    $env:SIGHTOCR_CONFIG = $previousConfig
    $env:SIGHTOCR_RESOURCES = $previousResources
}
$ocr = [IO.File]::ReadAllText($ocrOutput)
if (-not $ocr.Contains("Alpha`t100") -or -not $ocr.Contains("Beta`t200")) { throw 'Installed OneOCR resources did not produce the expected TSV.' }
$cliOcr = [IO.File]::ReadAllText($cliOutput) | ConvertFrom-Json
if ($cliOcr.provider -ne 'local' -or -not $cliOcr.text.Contains("Alpha`t100") -or -not $cliOcr.text.Contains("Beta`t200")) { throw 'Installed console CLI did not produce the expected local OCR JSON.' }
$uninstaller = Join-Path $installRoot 'unins000.exe'
Assert-TestPath $uninstaller
Run-Hidden $uninstaller @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', ('/LOG="' + (Join-Path $testRoot 'uninstall.log') + '"'))
foreach ($relative in $payload) {
    if (Test-Path -LiteralPath (Join-Path $installRoot $relative)) { throw "Uninstaller left an installed file: $relative" }
}
if ([IO.File]::ReadAllText($configSentinel) -ne $configText) { throw 'Uninstallation removed user-created configuration.' }
if (Test-Path -LiteralPath $smokeRegistryPath) { throw 'Smoke uninstall registration was left behind.' }
[pscustomobject]@{
    Complete = $true
    Version = $version
    TestRoot = $testRoot
    Compiler = $IsccPath
    InstallerSHA256 = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash
    PayloadSHA256 = $hashes
    InstallAndReinstall = $true
    Uninstall = $true
    PreservedConfiguration = $true
    NoUninstallRegistration = $true
    NoSilentLaunch = $true
    InstalledOneOcrTsv = $ocr
    InstalledConsoleOcrJson = $true
    ActiveMcpUpdateFailsPromptlyWithoutReplacement = $true
    ShutdownProtocol = 'shutdown/report.json'
    VariantLimit = 'Same payload and install/uninstall code; test variant excludes shortcuts and uninstall registration.'
} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $testRoot 'report.json') -Encoding UTF8
Write-Host "Installer smoke report: $testRoot/report.json"
