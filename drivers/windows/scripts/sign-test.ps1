<#
.SYNOPSIS
  DEVELOPMENT ONLY: test-signs the driver package with a self-signed certificate and trusts that
  certificate on this machine. Requires an elevated PowerShell and test-signing mode:
      bcdedit /set testsigning on     (then reboot; Secure Boot must be off, or use a VM)
  Release builds are attestation-signed through Microsoft Partner Center instead (docs/CICD.md).
#>
param([string] $Package = "$PSScriptRoot\..\out\x64")
$ErrorActionPreference = "Stop"
$subject = "CN=Annex Test Driver Signing"
$cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object Subject -eq $subject | Select-Object -First 1
if (-not $cert) {
  $cert = New-SelfSignedCertificate -Subject $subject -Type CodeSigningCert -CertStoreLocation Cert:\CurrentUser\My `
    -KeyExportPolicy Exportable -NotAfter (Get-Date).AddYears(2)
}
$cer = Join-Path $env:TEMP "annex-test-driver.cer"
Export-Certificate -Cert $cert -FilePath $cer | Out-Null
Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
$signtool = (Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
  Sort-Object FullName | Select-Object -Last 1).FullName
if (-not $signtool) { throw "signtool.exe not found (install the Windows SDK)" }
foreach ($f in (Join-Path $Package "AnnexIdd.dll"), (Join-Path $Package "AnnexIdd.cat")) {
  & $signtool sign /fd SHA256 /sha1 $cert.Thumbprint $f
  if ($LASTEXITCODE) { throw "signing $f failed" }
}
Write-Host "Test-signed $Package. Install with: annex-driver install `"$Package\AnnexIdd.inf`""
