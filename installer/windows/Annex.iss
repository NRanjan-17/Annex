; Annex Windows installer (Inno Setup 6.3+).
;
; Build (see scripts/package-windows.ps1):
;   iscc /DAppVersion=0.1.0 /DArch=x64 /DSourceDir=..\..\dist\win-x64\app /DOutputDir=..\..\dist installer\windows\Annex.iss
;
; - The user can choose any folder on any drive (the directory page is always shown).
; - "Install for me only" needs no admin rights; "Install for all users" installs to Program Files
;   and adds Windows Firewall rules.
; - Uninstalling removes the app AND everything Annex stored: identity key, trusted devices,
;   settings and logs (%LOCALAPPDATA%\Annex), plus the firewall rules.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef FileVersion
  #define FileVersion AppVersion
#endif
#ifndef Arch
  #define Arch "x64"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist\win-" + Arch + "\app"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

#define AppName "Annex"
#define AppExe "Annex.exe"
#define AppPublisher "Nalinish Ranjan"
#define AppUrl "https://github.com/NRanjan-17/Annex"

[Setup]
; Never change AppId: it links upgrades and the uninstaller to earlier installs.
AppId={{6F4C2B1E-8A0D-4F57-9B3A-2E7D5C1A9F40}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
; Always let the user pick the folder/drive, even on upgrades.
DisableDirPage=no
UsePreviousAppDir=yes
DisableProgramGroupPage=yes
AllowNoIcons=yes
; Per-user by default; the user may elevate to install for all users.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
LicenseFile=..\..\LICENSE
OutputDir={#OutputDir}
OutputBaseFilename=Annex-Setup-{#AppVersion}-{#Arch}
SetupIconFile=..\..\apps\windows\src\Annex.App\Assets\Annex.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Annex artwork in the wizard (made by scripts/icon/build-icons.sh), 100 % and 200 % sizes.
WizardImageFile=wizard.bmp,wizard-2x.bmp
WizardSmallImageFile=wizard-small.bmp,wizard-small-2x.bmp
CloseApplications=force
RestartApplications=no
MinVersion=10.0.19041
VersionInfoVersion={#FileVersion}
VersionInfoProductName={#AppName}
VersionInfoCompany={#AppPublisher}
#if Arch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
#ifexist SourceDir + "\driver\AnnexIdd.inf"
; The virtual display driver for Extend mode. Off by default; needs an all-users (administrator) install.
; driver-trust.ps1 signs it on this PC with a one-off certificate whose private key is deleted straight
; away, so Windows loads it without test signing.
Name: "extenddriver"; Description: "Install the Extend-mode display driver (lets this PC extend its display onto another computer)"; GroupDescription: "Extend mode:"; Flags: unchecked; Check: IsAdminInstallMode
#endif

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
#ifexist SourceDir + "\driver\AnnexIdd.inf"
Source: "driver-trust.ps1"; DestDir: "{app}\driver"; Flags: ignoreversion
#endif
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; "Start with Windows" (set from the app's General page) lives in the user's Run key; remove it on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Annex"; Flags: uninsdeletevalue dontcreatekey

[Run]
#ifexist SourceDir + "\driver\AnnexIdd.inf"
; Signs the driver for this PC, then installs it (only if signing worked).
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\driver\driver-trust.ps1"" -Action install -Package ""{app}\driver"""; Flags: runhidden waituntilterminated; Tasks: extenddriver; StatusMsg: "Installing the Extend-mode display driver..."
#endif
; Firewall: allow Annex on private networks (discovery + incoming sessions). All-users installs only;
; per-user installs get the standard Windows prompt on first use instead.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""Annex"" dir=in action=allow program=""{app}\{#AppExe}"" profile=private,domain enable=yes"; Flags: runhidden; Check: IsAdminInstallMode; StatusMsg: "Adding firewall rule..."
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; In-app updates run Setup silently with /UPDATE=1: start Annex again afterwards, as the signed-in user.
Filename: "{app}\{#AppExe}"; Flags: nowait runasoriginaluser; Check: IsUpdateRun

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#AppExe}"; Flags: runhidden; RunOnceId: "KillAnnex"
; Removes the Extend-mode display device and driver package if they were installed (no-op otherwise).
Filename: "{app}\annex-driver.exe"; Parameters: "uninstall"; Flags: runhidden waituntilterminated; Check: IsAdminInstallMode; RunOnceId: "RemoveDriver"
; ...and the certificate that signed it on this PC.
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\driver\driver-trust.ps1"" -Action uninstall"; Flags: runhidden waituntilterminated; Check: IsAdminInstallMode; RunOnceId: "RemoveDriverTrust"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Annex"""; Flags: runhidden; Check: IsAdminInstallMode; RunOnceId: "DelFirewall"

[UninstallDelete]
; Everything Annex stores for the user who runs the uninstaller: identity key, trusted devices,
; settings, logs and caches.
Type: filesandordirs; Name: "{localappdata}\Annex"
Type: filesandordirs; Name: "{userappdata}\Annex"
; Leftovers in the install folder (e.g. crash dumps, files written after install).
Type: filesandordirs; Name: "{app}"

[Code]
// Annex's in-app updater passes /UPDATE=1 (with /SILENT).
function IsUpdateRun: Boolean;
begin
  Result := ExpandConstant('{param:UPDATE|0}') = '1';
end;

const
  // Inno Setup's uninstall registration for this AppId (HKLM for all users, HKCU for one user).
  UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{6F4C2B1E-8A0D-4F57-9B3A-2E7D5C1A9F40}_is1';

function AllUsersInstallExists: Boolean;
var
  Path: String;
begin
  Result := RegQueryStringValue(HKLM64, UninstallKey, 'Inno Setup: App Path', Path)
    or RegQueryStringValue(HKLM32, UninstallKey, 'Inno Setup: App Path', Path);
end;

// An update must land on the existing install. Annex 0.1.1's updater could start Setup for the
// current user although Annex is installed for all users (e.g. on another drive), which made a
// second copy. Hand the update over to an all-users Setup instead.
function InitializeSetup: Boolean;
var
  Params: String;
  I, ResultCode: Integer;
begin
  Result := True;
  if IsUpdateRun and not IsAdminInstallMode and AllUsersInstallExists then
  begin
    Params := '';
    for I := 1 to ParamCount do
      if (CompareText(ParamStr(I), '/CURRENTUSER') <> 0) and (CompareText(Copy(ParamStr(I), 1, 5), '/DIR=') <> 0) then
        Params := Params + ' ' + AddQuotes(ParamStr(I));
    if ShellExec('runas', ExpandConstant('{srcexe}'), Params + ' /ALLUSERS', '', SW_SHOW, ewNoWait, ResultCode) then
      Result := False; // the all-users Setup does the update
  end;
end;

// After an all-users install, remove a per-user copy of Annex left by that old updater: its
// program files, shortcuts and registration. Settings, keys and paired devices are shared by both
// copies and stay.
procedure RemovePerUserCopy();
var
  Dir: String;
begin
  if not RegQueryStringValue(HKCU, UninstallKey, 'Inno Setup: App Path', Dir) then exit;
  if CompareText(RemoveBackslashUnlessRoot(Dir), RemoveBackslashUnlessRoot(ExpandConstant('{app}'))) = 0 then exit;
  if FileExists(AddBackslash(Dir) + '{#AppExe}') then
    DelTree(Dir, True, True, True);
  DeleteFile(ExpandConstant('{userprograms}\{#AppName}.lnk'));
  DeleteFile(ExpandConstant('{userdesktop}\{#AppName}.lnk'));
  RegDeleteKeyIncludingSubkeys(HKCU, UninstallKey);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and IsAdminInstallMode then
    RemovePerUserCopy();
end;

// For all-users installs, also remove per-user data of every profile on this PC so no keys or
// trusted-device lists are left behind.
procedure RemoveAllProfilesData();
var
  FindRec: TFindRec;
  UsersDir, DataDir: String;
begin
  UsersDir := ExpandConstant('{sd}\Users');
  if FindFirst(UsersDir + '\*', FindRec) then
  begin
    try
      repeat
        if (FindRec.Attributes and FILE_ATTRIBUTE_DIRECTORY <> 0) and (FindRec.Name <> '.') and (FindRec.Name <> '..') then
        begin
          DataDir := UsersDir + '\' + FindRec.Name + '\AppData\Local\Annex';
          if DirExists(DataDir) then
            DelTree(DataDir, True, True, True);
        end;
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usPostUninstall) and IsAdminInstallMode then
    RemoveAllProfilesData();
end;
