# Prepare the official compiler in this workspace without shortcuts, associations,
# uninstall registration, PATH changes, or launching the compiler IDE.
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$toolsRoot = Join-Path $projectRoot 'target/tools'
$compilerRoot = Join-Path $toolsRoot 'innosetup-6.7.3'
$downloadPath = Join-Path $toolsRoot 'innosetup-6.7.3.exe'
$url = 'https://github.com/jrsoftware/issrc/releases/download/is-6_7_3/innosetup-6.7.3.exe'
$expectedHash = '9C73C3BAE7ED48D44112A0F48E66742C00090BDB5BEF71D9D3C056C66E97B732'
New-Item -ItemType Directory -Path $toolsRoot -Force | Out-Null
if (-not (Test-Path -LiteralPath $downloadPath)) {
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $downloadPath
}
if ((Get-FileHash -LiteralPath $downloadPath -Algorithm SHA256).Hash -ne $expectedHash) {
    throw 'Official Inno Setup SHA-256 mismatch; compiler was not executed.'
}
$signature = Get-AuthenticodeSignature -LiteralPath $downloadPath
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'CN=Pyrsys B\.V\.') {
    throw 'Official Inno Setup publisher signature could not be validated.'
}
$compilerArguments = @(
    '/SP-', '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER',
    '/PORTABLE=1', '/NOICONS', '/TASKS=', '/LANG=english',
    ('/DIR="' + $compilerRoot + '"'),
    ('/LOG="' + (Join-Path $toolsRoot 'inno-prepare.log') + '"')
)
$process = Start-Process -FilePath $downloadPath -ArgumentList $compilerArguments -WindowStyle Hidden -PassThru -Wait
if ($process.ExitCode -ne 0) { throw "Compiler preparation failed ($($process.ExitCode))." }
$compilerPath = Join-Path $compilerRoot 'ISCC.exe'
if (-not (Test-Path -LiteralPath $compilerPath -PathType Leaf)) { throw 'ISCC.exe is missing.' }
[pscustomobject]@{
    Version = '6.7.3'
    Source = $url
    SHA256 = $expectedHash
    Authenticode = $signature.Status.ToString()
    Publisher = $signature.SignerCertificate.Subject
    ISCC = $compilerPath
} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $toolsRoot 'inno-provenance.json') -Encoding UTF8
Write-Host "Compiler: $compilerPath"
