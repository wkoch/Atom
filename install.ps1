# Instala o binário `atom.exe` (Windows) em %LOCALAPPDATA%\atom\bin.
# Uso:  powershell -ExecutionPolicy Bypass -File install.ps1
#       $env:ATOM_VERSION = "v0.9.0"; .\install.ps1
$ErrorActionPreference = "Stop"

$repo = if ($env:ATOM_REPO) { $env:ATOM_REPO } else { "wkoch/Atom" }
$bindir = if ($env:ATOM_INSTALL_DIR) { $env:ATOM_INSTALL_DIR } else { "$env:LOCALAPPDATA\atom\bin" }
$platform = "windows-amd64"

if ($env:ATOM_VERSION) {
  $tag = $env:ATOM_VERSION
} else {
  $latest = Invoke-WebRequest "https://github.com/$repo/releases/latest" -MaximumRedirection 0 -SkipHttpErrorCheck
  $tag = $latest.Headers.Location -replace ".*/", ""
}
if (-not $tag) { throw "não achei a tag da release" }

$url = if ($env:ATOM_DIST_URL) { $env:ATOM_DIST_URL } else { "https://github.com/$repo/releases/download/$tag/atom-$tag-$platform.zip" }
Write-Host "baixando $url"

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("atom-" + [IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  $zip = Join-Path $tmp "atom.zip"
  Invoke-WebRequest $url -OutFile $zip
  Expand-Archive $zip -DestinationPath (Join-Path $tmp "x")
  $bin = Get-ChildItem -Recurse -Filter "atom.exe" (Join-Path $tmp "x") | Select-Object -First 1
  if (-not $bin) { throw "zip sem binário atom.exe" }
  New-Item -ItemType Directory -Force -Path $bindir | Out-Null
  Copy-Item $bin.FullName (Join-Path $bindir "atom.exe") -Force
  Write-Host "atom $tag instalado em $bindir\atom.exe"
  & (Join-Path $bindir "atom.exe") version
  if (($env:PATH -split ";") -notcontains $bindir) {
    Write-Host "adicione ao PATH: `$env:PATH += `";$bindir`""
  }
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
