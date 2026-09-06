@echo off
setlocal DisableDelayedExpansion

net session >nul 2>&1
if %errorLevel% NEQ 0 (
    echo Requesting administrative privileges...
    powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
    exit /b
)

set "TARGET_DIR=%~dp0..\resources\oneocr"
for %%I in ("%TARGET_DIR%") do set "TARGET_DIR=%%~fI"
if not exist "%TARGET_DIR%" mkdir "%TARGET_DIR%"
if errorlevel 1 (
    echo Failed to create "%TARGET_DIR%".
    exit /b 1
)

echo Extracting Snipping Tool OCR Files...
echo.

set "PS_CMD=$ErrorActionPreference = 'Stop'; $pkg = Get-AppxPackage -Name 'Microsoft.ScreenSketch' -AllUsers | Sort-Object Version -Descending | Select-Object -First 1; if (-not $pkg) { throw 'Snipping Tool (Microsoft.ScreenSketch) app not found on this system.' }; $srcDir = Join-Path $pkg.InstallLocation 'SnippingTool'; $files = @('oneocr.dll', 'onnxruntime.dll', 'oneocr.onemodel'); foreach ($f in $files) { $srcFile = Join-Path $srcDir $f; if (-not (Test-Path -LiteralPath $srcFile -PathType Leaf)) { throw ('Required file missing: ' + $srcFile) }; Copy-Item -LiteralPath $srcFile -Destination $env:TARGET_DIR -Force; $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $env:TARGET_DIR $f)).Hash; Write-Host ('[+] Copied: ' + $f + '  SHA256=' + $hash) -ForegroundColor Green }"

powershell -NoProfile -ExecutionPolicy Bypass -Command "%PS_CMD%"
if errorlevel 1 (
    echo.
    echo Extraction failed. No successful build should use a partial resource set.
    pause
    exit /b 1
)

echo.
echo Done.
pause
