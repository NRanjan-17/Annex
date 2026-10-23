<#
.SYNOPSIS
  Restores the WDK/SDK NuGet packages and builds AnnexIdd.dll + AnnexIdd.inf (+ .cat with Inf2Cat).
.EXAMPLE
  drivers\windows\scripts\build.ps1 -Platform x64 -Configuration Release -Version 0.1.0
#>
param(
  [ValidateSet("x64", "ARM64")] [string] $Platform = "x64",
  [string] $Configuration = "Release",
  [string] $Version = "0.1.0"
)
$ErrorActionPreference = "Stop"
# Driver versions are numeric (a.b.c.d); drop any pre-release suffix.
$Version = ($Version -split '[-+]')[0]
# The fourth field is unique per build (hours since 2026), so Windows always treats a new
# driver as an update instead of keeping the one it already has loaded.
$Build = [int][Math]::Floor(((Get-Date).ToUniversalTime() - [datetime]'2026-01-01').TotalHours) % 65535
$Root = Resolve-Path "$PSScriptRoot\.."
$Packages = Join-Path $Root "packages"
nuget restore (Join-Path $Root "packages.config") -PackagesDirectory $Packages -NonInteractive
if ($LASTEXITCODE) { throw "nuget restore failed" }

$Wdk = Join-Path $Packages "Microsoft.Windows.WDK.$Platform.10.0.28000.2526\c"
$Kit = "10.0.28000.0"
msbuild (Join-Path $Root "AnnexIdd\AnnexIdd.vcxproj") /m /nologo /p:Configuration=$Configuration /p:Platform=$Platform `
  "/p:StampInfDriverVersion=$Version.$Build"
if ($LASTEXITCODE) { throw "msbuild failed" }

$Out = Join-Path $Root "AnnexIdd\$Platform\$Configuration"
$Pkg = Join-Path $Root "out\$Platform"
New-Item -ItemType Directory -Force $Pkg | Out-Null
$Built = Get-ChildItem $Out -Recurse -Include AnnexIdd.dll, AnnexIdd.inf | Sort-Object LastWriteTime -Descending
foreach ($name in "AnnexIdd.dll", "AnnexIdd.inf") {
  $f = $Built | Where-Object Name -eq $name | Select-Object -First 1
  if (-not $f) { throw "$name not produced" }
  Copy-Item $f.FullName $Pkg -Force
}
$Inf2Cat = Join-Path $Wdk "bin\$Kit\x86\Inf2Cat.exe"
$os = if ($Platform -eq "ARM64") { "10_RS3_ARM64" } else { "10_X64" }
& $Inf2Cat /driver:$Pkg /os:$os /uselocaltime
if ($LASTEXITCODE) { throw "Inf2Cat failed" }
Write-Host "Driver package (unsigned): $Pkg"
