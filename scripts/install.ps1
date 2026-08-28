# MemVault Windows installer.
#
# Downloads the x86_64 Windows release zip from GitHub Releases, verifies its
# SHA-256 against the release's SHA256SUMS, extracts the binaries into
# %LOCALAPPDATA%\memvault\bin, and prints PATH instructions.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\install.ps1
#
# Parameters:
#   -Version  release tag, e.g. v0.2.0 (default: latest)
#   -Prefix   install dir (default: %LOCALAPPDATA%\memvault\bin)
#
param(
  [string]$Version = "latest",
  [string]$Prefix = "$env:LOCALAPPDATA\memvault\bin"
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = "dreamor/memvault"
$target = "x86_64-pc-windows-msvc"

if ($env:PROCESSOR_ARCHITECTURE -notmatch "AMD64") {
  Write-Host "Only x86_64 Windows is currently supported (found $env:PROCESSOR_ARCHITECTURE)." -ForegroundColor Yellow
  exit 1
}

$archive = "memvault-$target.zip"
if ($Version -eq "latest") {
  $url = "https://github.com/$Repo/releases/latest/download/$archive"
  $sumsUrl = "https://github.com/$Repo/releases/latest/download/SHA256SUMS"
} else {
  $url = "https://github.com/$Repo/releases/download/$Version/$archive"
  $sumsUrl = "https://github.com/$Repo/releases/download/$Version/SHA256SUMS"
}

$tmp = Join-Path $env:TEMP ("memvault-install-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
  $zip = Join-Path $tmp $archive
  Write-Host "Downloading $Repo $Version ($target)..."
  Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing

  Write-Host "Verifying SHA-256..."
  $sums = (Invoke-WebRequest -Uri $sumsUrl -UseBasicParsing).Content
  $expectedLine = ($sums -split "`n" | Where-Object { $_ -match [regex]::Escape($archive) } | Select-Object -First 1)
  if (-not $expectedLine) { throw "No SHA-256 entry found for $archive" }
  $expected = (($expectedLine -split "\s+")[0]).Trim().ToLower()
  $actual = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLower()
  if ($actual -ne $expected) { throw "Checksum mismatch for $archive (expected $expected, got $actual)" }

  Expand-Archive -Path $zip -DestinationPath $tmp -Force
  New-Item -ItemType Directory -Path $Prefix -Force | Out-Null
  foreach ($bin in @("memvault-cli.exe", "memvault-mcp.exe", "memvault-proxy.exe")) {
    Copy-Item (Join-Path $tmp "memvault-$target\$bin") (Join-Path $Prefix $bin) -Force
  }

  Write-Host ""
  Write-Host "Installed to: $Prefix" -ForegroundColor Green
  Write-Host "Add to PATH:  setx PATH `"$Prefix;$env:PATH`"" -ForegroundColor Cyan
  Write-Host "Usage:        memvault --help / memvault-mcp --help"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
