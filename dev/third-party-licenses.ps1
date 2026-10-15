<#
.SYNOPSIS
  Generates the THIRD_PARTY_LICENSES text file for the Windows installer.

.DESCRIPTION
  Runs cargo-about (https://github.com/EmbarkStudios/cargo-about) against about.hbs
  and appends the hand-maintained notices in licenses/ (parakeet.cpp, ggml, assets).
  Depends only on this repository and cargo-about on PATH.

.PARAMETER Out
  Output file. Relative paths resolve against the repository root.
  Defaults to target\THIRD_PARTY_LICENSES.txt (untracked build output).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File dev\third-party-licenses.ps1
#>
param(
    [string]$Out = 'target\THIRD_PARTY_LICENSES.txt'
)

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
Push-Location $repo
try {
    if (-not (Get-Command cargo-about -ErrorAction SilentlyContinue)) {
        throw 'cargo-about not found on PATH. Install with: cargo install cargo-about --locked --features cli'
    }

    $outPath = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $repo $Out }
    $outDir = Split-Path -Parent $outPath
    if ($outDir -and -not (Test-Path $outDir)) {
        New-Item -ItemType Directory -Force -Path $outDir | Out-Null
    }

    $generated = Join-Path ([System.IO.Path]::GetTempPath()) ('cluelyrs-about-' + [guid]::NewGuid().ToString('N') + '.txt')
    try {
        cargo about generate --fail about.hbs -o $generated
        if ($LASTEXITCODE -ne 0) {
            throw "cargo about generate failed with exit code $LASTEXITCODE"
        }

        $separator = '=' * 80
        $parts = New-Object System.Collections.Generic.List[string]
        $parts.Add((Get-Content -Raw -Encoding UTF8 $generated).TrimEnd())

        $notices = @(
            @{ File = 'parakeet.cpp.txt'; Title = 'parakeet.cpp (MIT License)' },
            @{ File = 'ggml.txt'; Title = 'ggml (MIT License)' },
            @{ File = 'assets.txt'; Title = 'Icons and logos (Lucide, Lobe Icons, Simple Icons)' }
        )
        foreach ($n in $notices) {
            $src = Join-Path $repo (Join-Path 'licenses' $n.File)
            if (-not (Test-Path $src)) {
                throw "Missing license notice: $src"
            }
            $parts.Add("$separator`n$($n.Title)`n$separator`n" + (Get-Content -Raw -Encoding UTF8 $src).TrimEnd())
        }

        $text = ($parts -join "`n`n") + "`n"
        [System.IO.File]::WriteAllText($outPath, $text.Replace("`r`n", "`n").Replace("`n", "`r`n"), (New-Object System.Text.UTF8Encoding($false)))
    }
    finally {
        if (Test-Path $generated) { [System.IO.File]::Delete($generated) }
    }

    Write-Host "Wrote $outPath"
}
finally {
    Pop-Location
}