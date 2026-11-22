# SailTerm Windows packaging: check NSIS toolchain -> tauri build -> copy installer
# Usage: powershell -ExecutionPolicy Bypass -File scripts\package.ps1
$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$nsis = Join-Path $env:LOCALAPPDATA "tauri\NSIS"
$required = @(
  "makensis.exe", "Bin\makensis.exe", "Stubs\lzma-x86-unicode", "Stubs\lzma_solid-x86-unicode",
  "Plugins\x86-unicode\additional\nsis_tauri_utils.dll"
)
$missing = @($required | Where-Object { -not (Test-Path (Join-Path $nsis $_)) })
if ($missing.Count -gt 0) {
  Write-Host "NSIS toolchain incomplete, missing:"
  $missing | ForEach-Object { Write-Host ("  " + $_) }
  Write-Host "See RELEASE.md to preinstall %LOCALAPPDATA%\tauri\NSIS"
  exit 1
}

# 360 may block release build-scripts: use a plainer profile and retry
$env:CARGO_TARGET_DIR = Join-Path $env:USERPROFILE ".cache\cargo-target-sailterm"
$env:CARGO_PROFILE_RELEASE_LTO = "false"
$env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "16"
$env:CARGO_PROFILE_RELEASE_STRIP = "none"
$env:CARGO_PROFILE_RELEASE_PANIC = "unwind"

$log = Join-Path $root "package-build.log"
$code = 1
for ($i = 1; $i -le 8; $i++) {
  Write-Host ("=== tauri build try " + $i + " ===")
  $p = Start-Process -FilePath "cmd.exe" -ArgumentList "/c", "pnpm tauri build --bundles nsis > `"$log`" 2>&1" -Wait -PassThru -NoNewWindow
  $code = $p.ExitCode
  if ($code -eq 0) { break }
  Write-Host ("exit=" + $code)
  if (Test-Path $log) {
    Get-Content $log | Select-String -Pattern "could not execute|error:|Finished 1 bundle" |
      Select-Object -First 4 | ForEach-Object { Write-Host ("  " + $_.Line.Trim()) }
  }
  Start-Sleep -Seconds 2
}
if ($code -ne 0) {
  Write-Host ("pack failed, log: " + $log)
  exit 1
}

$bundleDir = Join-Path $env:CARGO_TARGET_DIR "release\bundle\nsis"
$setup = Get-ChildItem -Path $bundleDir -Filter "sailterm_*-setup.exe" -ErrorAction SilentlyContinue |
  Sort-Object LastWriteTime -Descending |
  Select-Object -First 1

if (-not $setup) {
  Write-Host ("setup.exe not found, log: " + $log)
  exit 1
}

$dest = Join-Path $root "dist-bundle"
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item $setup.FullName $dest -Force
Write-Host ("OK " + $setup.FullName)
Write-Host ("copied to dist-bundle, size_MB=" + [math]::Round($setup.Length / 1MB, 1))
