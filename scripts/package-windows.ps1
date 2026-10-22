<#
.SYNOPSIS
  Builds the Rust core and the WinUI app for one architecture and packages them with Inno Setup.
.EXAMPLE
  scripts\package-windows.ps1 -Arch x64 -Version 0.1.0
.NOTES
  Requires: Rust (rustup), .NET SDK (global.json), Inno Setup 6.3+ (iscc.exe on PATH or default location).
  Signing: set ANNEX_SIGN_CMD to a command that signs the file passed as its last argument
  (e.g. a signtool or Azure Trusted Signing invocation); otherwise binaries are unsigned.
#>
param(
  [ValidateSet("x64", "arm64")] [string] $Arch = "x64",
  [string] $Version = ""
)
$ErrorActionPreference = "Stop"
$Root = Resolve-Path "$PSScriptRoot\.."
if (-not $Version) {
  $Version = (Select-String -Path "$Root\core\Cargo.toml" -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
}
$Triple = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$Platform = if ($Arch -eq "arm64") { "ARM64" } else { "x64" }
$Rid = "win-$Arch"
$Out = "$Root\dist\win-$Arch\app"

function Invoke-Sign([string] $File) {
  if ($env:ANNEX_SIGN_CMD) { & cmd /c "$env:ANNEX_SIGN_CMD `"$File`""; if ($LASTEXITCODE) { throw "signing failed: $File" } }
}

Write-Host "==> Rust core ($Triple)"
rustup target add $Triple | Out-Null
cargo build --manifest-path "$Root\core\Cargo.toml" -p annex-ffi --release --locked --target $Triple
if ($LASTEXITCODE) { throw "cargo build failed" }

Write-Host "==> WinUI app ($Platform)"
if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
dotnet publish "$Root\apps\windows\src\Annex.App\Annex.App.csproj" -c Release -p:Platform=$Platform -r $Rid `
  -p:Version=$Version -p:SelfContained=true -o $Out
if ($LASTEXITCODE) { throw "dotnet publish failed" }
if (-not (Test-Path "$Out\annex_ffi.dll")) { throw "annex_ffi.dll missing from $Out" }
Get-ChildItem $Out -Include Annex.exe, Annex.dll, annex_ffi.dll -Recurse | ForEach-Object { Invoke-Sign $_.FullName }

Write-Host "==> Installer"
$Iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
if (-not $Iscc) { $Iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
& $Iscc "/DAppVersion=$Version" "/DArch=$Arch" "/DSourceDir=$Out" "/DOutputDir=$Root\dist" "$Root\installer\windows\Annex.iss"
if ($LASTEXITCODE) { throw "iscc failed" }
$Setup = "$Root\dist\Annex-Setup-$Version-$Arch.exe"
Invoke-Sign $Setup
(Get-FileHash -Algorithm SHA256 $Setup).Hash.ToLower() + "  " + (Split-Path $Setup -Leaf) | Set-Content "$Setup.sha256"
Write-Host "==> Done: $Setup"
