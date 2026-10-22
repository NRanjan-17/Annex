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
# Windows file versions must be numeric (a.b.c); keep any pre-release suffix only in names.
$NumericVersion = ($Version -split '[-+]')[0]
$Triple = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$Platform = if ($Arch -eq "arm64") { "ARM64" } else { "x64" }
$Rid = "win-$Arch"
$Out = "$Root\dist\win-$Arch\app"

function Invoke-Sign([string] $File) {
  if ($env:ANNEX_SIGN_CMD) { & cmd /c "$env:ANNEX_SIGN_CMD `"$File`""; if ($LASTEXITCODE) { throw "signing failed: $File" } }
}

Write-Host "==> Rust core ($Triple)"
rustup target add $Triple | Out-Null
cargo build --manifest-path "$Root\core\Cargo.toml" -p annex-ffi -p annex-driver --release --locked --target $Triple
if ($LASTEXITCODE) { throw "cargo build failed" }

Write-Host "==> WinUI app ($Platform)"
if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
dotnet publish "$Root\apps\windows\src\Annex.App\Annex.App.csproj" -c Release -p:Platform=$Platform -r $Rid `
  -p:Version=$NumericVersion -p:InformationalVersion=$Version -p:SelfContained=true -o $Out
if ($LASTEXITCODE) { throw "dotnet publish failed" }
if (-not (Test-Path "$Out\annex_ffi.dll")) { throw "annex_ffi.dll missing from $Out" }
Copy-Item "$Root\core\target\$Triple\release\annex-driver.exe" $Out -Force

# Extend-mode driver (built by drivers\windows\scripts\build.ps1). Optional: without it the installer has no
# driver option and Windows hosts offer Mirror only.
$DriverPkg = "$Root\drivers\windows\out\" + $(if ($Arch -eq "arm64") { "ARM64" } else { "x64" })
if (Test-Path "$DriverPkg\AnnexIdd.inf") {
  New-Item -ItemType Directory -Force "$Out\driver" | Out-Null
  Copy-Item "$DriverPkg\*" "$Out\driver" -Force
  Write-Host "==> Including the Extend-mode driver package from $DriverPkg"
} else {
  Write-Host "==> No driver package at $DriverPkg (Extend mode driver not included)"
}
Get-ChildItem $Out -Include Annex.exe, Annex.dll, annex_ffi.dll, annex-driver.exe -Recurse | ForEach-Object { Invoke-Sign $_.FullName }

Write-Host "==> Installer"
$Iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
if (-not $Iscc) { $Iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
& $Iscc "/DAppVersion=$Version" "/DFileVersion=$NumericVersion" "/DArch=$Arch" "/DSourceDir=$Out" "/DOutputDir=$Root\dist" "$Root\installer\windows\Annex.iss"
if ($LASTEXITCODE) { throw "iscc failed" }
$Setup = "$Root\dist\Annex-Setup-$Version-$Arch.exe"
Invoke-Sign $Setup
(Get-FileHash -Algorithm SHA256 $Setup).Hash.ToLower() + "  " + (Split-Path $Setup -Leaf) | Set-Content "$Setup.sha256"
Write-Host "==> Done: $Setup"
