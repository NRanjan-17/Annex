# Installs Annex silently (per user), launches it and checks that it starts properly: the process
# stays alive, keeps responding and shows its main window. Collects evidence in -OutDir:
# summary.txt, crash.log, Windows event-log errors, managed stacks of a hung process, a screenshot.
param(
    [Parameter(Mandatory)] [string] $Installer,
    [string] $OutDir = "smoke",
    [int] $WaitSeconds = 25
)
$ErrorActionPreference = "Continue"
New-Item -ItemType Directory -Force $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$dir = "$env:ProgramFiles\AnnexSmoke"
$log = Join-Path $OutDir "summary.txt"
function Note($text) { $text | Tee-Object -FilePath $log -Append }

Note "Installing $Installer"
# All users (the runner is an administrator) with the Extend-mode driver, like a full install.
$p = Start-Process $Installer -ArgumentList "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/ALLUSERS", "/TASKS=`"extenddriver`"", "/DIR=`"$dir`"", "/LOG=`"$OutDir\install.log`"" -PassThru
# Never wait forever: a hidden dialog would hang the job.
if (-not $p.WaitForExit(300000)) { Note "FAIL: installer still running after 5 minutes"; Stop-Process -Id $p.Id -Force }
Note "Installer exit code: $($p.ExitCode)"
$trustLog = "$env:ProgramData\Annex\driver-trust.log"
if (Test-Path $trustLog) { Copy-Item $trustLog $OutDir; Get-Content $trustLog | Select-String "\] " | ForEach-Object { Note ("driver: " + $_.Line) } }
$exe = Join-Path $dir "Annex.exe"
if (-not (Test-Path $exe)) { Note "FAIL: $exe missing after install"; exit 1 }
Get-ChildItem $dir | Select-Object Name, Length | Out-File (Join-Path $OutDir "installed-files.txt")

$started = Get-Date
# Self-test mode: the app also opens every page twice and leaves a marker file when done.
$env:ANNEX_SELFTEST = "1"
$marker = "$env:LOCALAPPDATA\Annex\selftest-ok"
Remove-Item $marker -ErrorAction SilentlyContinue
$app = Start-Process $exe -PassThru
Note "Launched Annex.exe (pid $($app.Id))"
Start-Sleep -Seconds $WaitSeconds

$alive = -not $app.HasExited
$app.Refresh()
$responding = $alive -and $app.Responding
$window = if ($alive) { $app.MainWindowHandle } else { 0 }
Note "Alive: $alive  Responding: $responding  MainWindowHandle: $window  Title: '$($app.MainWindowTitle)'"
if (-not $alive) { Note "Exit code: $($app.ExitCode)" }

# Screenshot of the whole desktop.
try {
    Add-Type -AssemblyName System.Windows.Forms, System.Drawing
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    [System.Drawing.Graphics]::FromImage($bmp).CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $bmp.Save((Join-Path $OutDir "screenshot.png"))
} catch { Note "Screenshot failed: $_" }

# Managed stacks: shows where a hung start-up is stuck.
if ($alive) {
    dotnet tool install --global dotnet-stack 2>&1 | Out-Null
    & "$env:USERPROFILE\.dotnet\tools\dotnet-stack.exe" report -p $app.Id 2>&1 | Out-File (Join-Path $OutDir "stacks.txt")
}

$crash = "$env:LOCALAPPDATA\Annex\crash.log"
if (Test-Path $crash) { Copy-Item $crash $OutDir; Note "crash.log found" }
Get-ChildItem "$env:LOCALAPPDATA\Annex" -ErrorAction SilentlyContinue | Select-Object Name, Length | Out-File (Join-Path $OutDir "data-dir.txt")
Get-WinEvent -FilterHashtable @{ LogName = 'Application'; StartTime = $started.AddMinutes(-1) } -ErrorAction SilentlyContinue |
    Where-Object { $_.LevelDisplayName -in 'Error', 'Critical', 'Warning' } |
    Format-List TimeCreated, ProviderName, Id, Message | Out-File (Join-Path $OutDir "events.txt")

if ($alive) { Stop-Process -Id $app.Id -Force }
$selftest = Test-Path $marker
# Extend-mode driver: did signing work, and does Windows run it?
$extend = "$env:LOCALAPPDATA\Annex\selftest-extend.json"
if (Test-Path $extend) {
    $ex = Get-Content $extend -Raw
    Note ("Extend status: " + $ex)
    Note ("EXTEND: " + $(if ($ex -match '"state":"ready"') { "READY (driver signed on this PC and running)" } else { "NOT READY" }))
}
pnputil /enum-devices /class Display 2>&1 | Out-File (Join-Path $OutDir "display-devices.txt")
Note "Self-test (every page opened twice): $selftest"
$ok = $alive -and $responding -and ($window -ne 0) -and $selftest -and -not (Test-Path $crash)
Note ($ok ? "PASS" : "FAIL")
exit ($ok ? 0 : 1)
