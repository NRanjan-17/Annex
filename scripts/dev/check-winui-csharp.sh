#!/bin/bash
# Compile-checks the WinUI app's C# (not its XAML) on macOS/Linux, where the XAML compiler and
# MakePri can't run. Stand-ins for XAML-generated members live in scripts/dev/WinUIStubs.cs.
# Usage: scripts/dev/check-winui-csharp.sh   (needs the .NET SDK)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
APP="$ROOT/apps/windows/src/Annex.App"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
VERSION_WASDK=$(sed -n 's/.*"Microsoft.WindowsAppSDK" Version="\([^"]*\)".*/\1/p' "$APP/Annex.App.csproj")
VERSION_BT=$(sed -n 's/.*"Microsoft.Windows.SDK.BuildTools" Version="\([^"]*\)".*/\1/p' "$APP/Annex.App.csproj")
{
  echo '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup>'
  echo '<TargetFramework>net10.0-windows10.0.19041.0</TargetFramework><EnableWindowsTargeting>true</EnableWindowsTargeting>'
  echo '<Nullable>enable</Nullable><ImplicitUsings>enable</ImplicitUsings><AllowUnsafeBlocks>true</AllowUnsafeBlocks>'
  echo '<TreatWarningsAsErrors>true</TreatWarningsAsErrors><RuntimeIdentifier>win-x64</RuntimeIdentifier>'
  echo '<EnableDefaultCompileItems>false</EnableDefaultCompileItems><WindowsAppSdkBootstrapInitialize>false</WindowsAppSdkBootstrapInitialize>'
  echo '<WindowsAppSDKSelfContained>false</WindowsAppSDKSelfContained><EnableCoreMrtTooling>false</EnableCoreMrtTooling><AppxGeneratePriEnabled>false</AppxGeneratePriEnabled>'
  echo '</PropertyGroup><ItemGroup>'
  echo "<PackageReference Include=\"Microsoft.WindowsAppSDK\" Version=\"$VERSION_WASDK\" />"
  echo "<PackageReference Include=\"Microsoft.Windows.SDK.BuildTools\" Version=\"$VERSION_BT\" />"
  echo "<ProjectReference Include=\"$ROOT/apps/windows/src/Annex.Interop/Annex.Interop.csproj\" />"
  for f in "$APP"/*.cs; do case "$f" in *App.xaml.cs) ;; *) echo "<Compile Include=\"$f\" />" ;; esac; done
  echo "<Compile Include=\"$ROOT/scripts/dev/WinUIStubs.cs\" />"
  echo '</ItemGroup></Project>'
} > "$WORK/check.csproj"
DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 dotnet build "$WORK/check.csproj" -nologo -v q
