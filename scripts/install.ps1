# Installs the latest Annex on Windows (PowerShell 5.1 or 7):
#   irm https://raw.githubusercontent.com/NRanjan-17/Annex/main/scripts/install.ps1 | iex
#
# Downloads the installer for this PC (x64 or ARM64), checks the release's SHA256SUMS.txt against
# its Ed25519 signature (the key the in-app updater trusts) and the installer against
# SHA256SUMS.txt, then runs it. Nothing runs if a check fails.
#
# $env:ANNEX_VERSION = '1.2.3' picks a version; $env:ANNEX_DRY_RUN = '1' downloads and checks
# but runs nothing; $env:ANNEX_BASE_URL points at the release files instead of GitHub (testing).
# Runs in its own scope, so nothing is left behind in the PowerShell session that ran it.
& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Add-Type -AssemblyName System.Numerics

    $Repo = 'NRanjan-17/Annex'
    # Signs every release's SHA256SUMS.txt (core/annex-update UPDATE_PUBLIC_KEY).
    $PublicKeyHex = '95ebd305c46941cb5b5dd30f8f9c1d3c2c7454327f29fd925a91f2044205dc36'

    function Fail([string]$Message) { throw "Annex: $Message" }

    # ---- Ed25519 signature check (RFC 8032), as Windows has none built in ----

    $P = [Numerics.BigInteger]::Pow(2, 255) - 19
    $L = [Numerics.BigInteger]::Pow(2, 252) + [Numerics.BigInteger]::Parse('27742317777372353535851937790883648493')
    function ModP([Numerics.BigInteger]$a) { $r = $a % $P; if ($r.Sign -lt 0) { $r + $P } else { $r } }
    function Inv([Numerics.BigInteger]$a) { [Numerics.BigInteger]::ModPow((ModP $a), $P - 2, $P) }
    $D = ModP ([Numerics.BigInteger]-121665 * (Inv 121666))
    $SqrtM1 = [Numerics.BigInteger]::ModPow(2, ($P - 1) / 4, $P)
    $Base = @(
        [Numerics.BigInteger]::Parse('15112221349535400772501151409588531511454012693041857206046113283949847762202'),
        [Numerics.BigInteger]::Parse('46316835694926478169428394003475163141307993866256225615783033603165251855960'))

    function LittleEndian([byte[]]$bytes) {
        # BigInteger reads little-endian two's complement: a trailing zero keeps it positive.
        [Numerics.BigInteger]::new([byte[]]($bytes + [byte]0))
    }

    function Add-Points($a, $b) {
        $xx = ModP ($a[0] * $b[0]); $yy = ModP ($a[1] * $b[1])
        $t = ModP ($D * $xx * $yy)
        @((ModP (($a[0] * $b[1] + $a[1] * $b[0]) * (Inv (1 + $t)))),
          (ModP (($yy + $xx) * (Inv (1 - $t)))))
    }

    function Multiply($point, [Numerics.BigInteger]$n) {
        $result = @([Numerics.BigInteger]::Zero, [Numerics.BigInteger]::One)
        while ($n.Sign -gt 0) {
            if (-not $n.IsEven) { $result = Add-Points $result $point }
            $point = Add-Points $point $point
            $n = $n / 2
        }
        $result
    }

    function Decode-Point([byte[]]$bytes) {
        $b = [byte[]]$bytes.Clone()
        $sign = ($b[31] -shr 7) -band 1
        $b[31] = $b[31] -band 0x7f
        $y = LittleEndian $b
        if ($y -ge $P) { return $null }
        $y2 = ModP ($y * $y)
        $x2 = ModP (($y2 - 1) * (Inv ($D * $y2 + 1)))
        $x = [Numerics.BigInteger]::ModPow($x2, ($P + 3) / 8, $P)
        if ((ModP ($x * $x - $x2)) -ne 0) { $x = ModP ($x * $SqrtM1) }
        if ((ModP ($x * $x - $x2)) -ne 0) { return $null }
        if ([int]($x % 2) -ne $sign) { $x = $P - $x }
        , @($x, $y)
    }

    function Test-Ed25519([byte[]]$publicKey, [byte[]]$message, [byte[]]$signature) {
        if ($signature.Length -ne 64 -or $publicKey.Length -ne 32) { return $false }
        $a = Decode-Point $publicKey
        $r = Decode-Point $signature[0..31]
        $s = LittleEndian $signature[32..63]
        if ($null -eq $a -or $null -eq $r -or $s -ge $L) { return $false }
        $sha = [Security.Cryptography.SHA512]::Create()
        $k = (LittleEndian $sha.ComputeHash([byte[]]($signature[0..31] + $publicKey + $message))) % $L
        $left = Multiply $Base $s
        $right = Add-Points $r (Multiply $a $k)
        ($left[0] -eq $right[0]) -and ($left[1] -eq $right[1])
    }

    # ---- Download, check, install ----

    $Version = $env:ANNEX_VERSION
    if (-not $Version) {
        $latest = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest" -Headers @{ 'User-Agent' = 'Annex-installer' }
        $Version = $latest.tag_name -replace '^v', ''
    }
    if ($Version -notmatch '^\d+\.\d+\.\d+') { Fail "couldn't find the latest version (got '$Version')" }
    $BaseUrl = if ($env:ANNEX_BASE_URL) { $env:ANNEX_BASE_URL } else { "https://github.com/$Repo/releases/download/v$Version" }

    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    $Arch = if ($arch -eq 'ARM64') { 'arm64' } else { 'x64' }
    $Name = "Annex-Setup-$Version-$Arch.exe"

    $Dir = Join-Path ([IO.Path]::GetTempPath()) ("Annex-Install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $Dir | Out-Null
    try {
        foreach ($f in @('SHA256SUMS.txt', 'SHA256SUMS.txt.sig', $Name)) {
            Invoke-WebRequest "$BaseUrl/$f" -OutFile (Join-Path $Dir $f) -UseBasicParsing
        }
        $sums = [IO.File]::ReadAllBytes((Join-Path $Dir 'SHA256SUMS.txt'))
        $sig = [IO.File]::ReadAllBytes((Join-Path $Dir 'SHA256SUMS.txt.sig'))
        $key = [byte[]] -split ($PublicKeyHex -replace '..', '0x$& ')
        if (-not (Test-Ed25519 $key $sums $sig)) { Fail "the release's signature doesn't verify; nothing was installed" }
        Write-Host "Checked the release's signature"

        $want = [Text.Encoding]::UTF8.GetString($sums) -split "`n" |
            Where-Object { ($_ -split '\s+')[1] -eq $Name } | ForEach-Object { ($_ -split '\s+')[0] }
        if (-not $want) { Fail "$Name isn't listed in SHA256SUMS.txt" }
        $got = (Get-FileHash (Join-Path $Dir $Name) -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($got -ne $want) { Fail "$Name doesn't match its checksum; nothing was installed" }
        Write-Host "Downloaded and checked $Name"

        if ($env:ANNEX_DRY_RUN -eq '1') {
            Write-Host "Would run $Name"
        } else {
            # The usual setup wizard: choose who it's for and the Extend display driver there.
            Start-Process (Join-Path $Dir $Name) -Wait
            Write-Host "Annex $Version is installed."
        }
    } finally {
        Remove-Item $Dir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
