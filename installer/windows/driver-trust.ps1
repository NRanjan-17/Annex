<#
.SYNOPSIS
  Signs the Annex display driver on this PC so Windows loads it without test signing, and removes
  that trust again on uninstall. Run elevated by the installer.

.DESCRIPTION
  AnnexIdd is an Indirect Display (IddCx) driver, which runs in user mode. Windows accepts a
  user-mode driver package whose catalog is signed by a certificate this PC trusts. So:

    1. create a code-signing certificate that exists only on this PC,
    2. sign the driver catalog (AnnexIdd.cat) with it,
    3. trust that certificate (Trusted Root + Trusted Publishers),
    4. delete its private key at once.

  Because the private key is gone, nothing else can ever be signed with this certificate, so
  trusting it can't be abused. Its thumbprint is kept in HKLM\SOFTWARE\Annex so uninstall can
  remove it.

  With -Action install it then installs the driver (annex-driver.exe, non-interactive), and only if
  signing worked, so an unsigned install can never be attempted.

.EXAMPLE
  driver-trust.ps1 -Action install -Package "C:\Program Files\Annex\driver"
  driver-trust.ps1 -Action uninstall
#>
param(
    [Parameter(Mandatory)] [ValidateSet("install", "uninstall")] [string] $Action,
    [string] $Package
)
$ErrorActionPreference = "Stop"
$RegPath = "HKLM:\SOFTWARE\Annex"
$RegValue = "DriverCertThumbprint"
$Log = Join-Path $env:ProgramData "Annex\driver-trust.log"
New-Item -ItemType Directory -Force (Split-Path $Log) | Out-Null
Start-Transcript -Path $Log -Append | Out-Null
function Note([string] $Text) { Write-Output "[$(Get-Date -Format o)] $Text" }
trap {
    Note "FAILED: $_"
    Stop-Transcript | Out-Null
    exit 1
}

function Remove-Trust {
    $thumb = (Get-ItemProperty $RegPath -Name $RegValue -ErrorAction SilentlyContinue).$RegValue
    if (-not $thumb) { return }
    foreach ($store in "Root", "TrustedPublisher", "My") {
        Get-ChildItem "Cert:\LocalMachine\$store\$thumb" -ErrorAction SilentlyContinue |
            Remove-Item -DeleteKey -ErrorAction SilentlyContinue
    }
    Remove-ItemProperty $RegPath -Name $RegValue -ErrorAction SilentlyContinue
    Note "removed driver certificate $thumb"
}

if ($Action -eq "uninstall") {
    Remove-Trust
    Stop-Transcript | Out-Null
    exit 0
}

$catalog = Join-Path $Package "AnnexIdd.cat"
if (-not (Test-Path $catalog)) { Note "no catalog at $catalog"; throw "AnnexIdd.cat missing from $Package" }

# A previous install's certificate is replaced, never accumulated.
Remove-Trust

$cert = New-SelfSignedCertificate -Type CodeSigningCert `
    -Subject "CN=Annex display driver (signed on $env:COMPUTERNAME)" `
    -CertStoreLocation Cert:\LocalMachine\My `
    -KeyAlgorithm RSA -KeyLength 3072 -HashAlgorithm SHA256 `
    -KeyExportPolicy NonExportable -NotAfter (Get-Date).AddYears(30)
try {
    # The catalog covers AnnexIdd.dll and AnnexIdd.inf, so signing it signs the package.
    $result = Set-AuthenticodeSignature -FilePath $catalog -Certificate $cert -HashAlgorithm SHA256
    Note "Set-AuthenticodeSignature: $($result.Status) $($result.StatusMessage)"
    if (-not $result.SignerCertificate) { throw "signing the catalog failed: $($result.Status) $($result.StatusMessage)" }

    $cer = Join-Path $env:TEMP "annex-driver-$($cert.Thumbprint).cer"
    Export-Certificate -Cert $cert -FilePath $cer | Out-Null
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
    Remove-Item $cer -Force
}
finally {
    # The private key never outlives this script.
    Remove-Item "Cert:\LocalMachine\My\$($cert.Thumbprint)" -DeleteKey -ErrorAction SilentlyContinue
}

New-Item -Path $RegPath -Force | Out-Null
Set-ItemProperty $RegPath -Name $RegValue -Value $cert.Thumbprint

$check = Get-AuthenticodeSignature -FilePath $catalog
Note "signed $catalog with $($cert.Thumbprint): $($check.Status) $($check.StatusMessage)"

# Install only now. annex-driver installs non-interactively: a bad signature fails instead of prompting.
$tool = Join-Path (Split-Path $Package) "annex-driver.exe"
$inf = Join-Path $Package "AnnexIdd.inf"
$output = & $tool install $inf 2>&1
$code = $LASTEXITCODE
Note "annex-driver install (exit $code): $output"
Stop-Transcript | Out-Null
exit $code
