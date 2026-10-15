param(
    [string]$Exe = 'target\release\cluely-rs.exe',
    [string]$Licenses = 'target\THIRD_PARTY_LICENSES.txt',
    [string]$Version
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

if (-not $Version) {
    $cargo = Get-Content (Join-Path $repo 'Cargo.toml') -Raw
    $m = [regex]::Match($cargo, '(?ms)^\[package\][^\[]*?^version\s*=\s*"([^"]+)"')
    if (-not $m.Success) { throw 'Could not read version from Cargo.toml [package].' }
    $Version = $m.Groups[1].Value
}

$iscc = $null
$cmd = Get-Command ISCC.exe -ErrorAction SilentlyContinue
if ($cmd) { $iscc = $cmd.Source }
if (-not $iscc) {
    foreach ($c in @(
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
    )) {
        if (Test-Path $c) { $iscc = $c; break }
    }
}
if (-not $iscc) { throw 'ISCC.exe not found. Install Inno Setup 6 (winget install --id JRSoftware.InnoSetup -e).' }

$exePath = (Resolve-Path -LiteralPath $Exe).Path
$licPath = (Resolve-Path -LiteralPath $Licenses).Path
$script = Join-Path $repo 'installer\CluelyRS.iss'

& $iscc "/DAppVersion=$Version" "/DAppExe=$exePath" "/DThirdPartyLicenses=$licPath" $script
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit code $LASTEXITCODE." }

$out = Join-Path $repo "target\installer\CluelyRS-$Version-setup.exe"
if (-not (Test-Path $out)) { throw "Expected installer not found: $out" }
Write-Host "Installer written to: $out"
