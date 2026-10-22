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
; The virtual display driver for Extend mode. Off by default: until the driver is attestation-signed it only loads
; with test signing enabled (bcdedit /set testsigning on). Needs an all-users (administrator) install.
Name: "extenddriver"; Description: "Install the Extend-mode display driver (lets other computers use this PC as an extra display; requires test signing until the driver is signed)"; GroupDescription: "Extend mode:"; Flags: unchecked; Check: IsAdminInstallMode
#endif

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; "Start with Windows" (set from the app's General page) lives in the user's Run key; remove it on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "Annex"; Flags: uninsdeletevalue dontcreatekey

[Run]
#ifexist SourceDir + "\driver\AnnexIdd.inf"
Filename: "{app}\annex-driver.exe"; Parameters: "install ""{app}\driver\AnnexIdd.inf"""; Flags: runhidden waituntilterminated; Tasks: extenddriver; StatusMsg: "Installing the Extend-mode display driver..."
#endif
; Firewall: allow Annex on private networks (discovery + incoming sessions). All-users installs only;
; per-user installs get the standard Windows prompt on first use instead.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""Annex"" dir=in action=allow program=""{app}\{#AppExe}"" profile=private,domain enable=yes"; Flags: runhidden; Check: IsAdminInstallMode; StatusMsg: "Adding firewall rule..."
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#AppExe}"; Flags: runhidden; RunOnceId: "KillAnnex"
; Removes the Extend-mode display device and driver package if they were installed (no-op otherwise).
Filename: "{app}\annex-driver.exe"; Parameters: "uninstall"; Flags: runhidden waituntilterminated; Check: IsAdminInstallMode; RunOnceId: "RemoveDriver"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Annex"""; Flags: runhidden; Check: IsAdminInstallMode; RunOnceId: "DelFirewall"

[UninstallDelete]
; Everything Annex stores for the user who runs the uninstaller: identity key, trusted devices,
; settings, logs and caches.
Type: filesandordirs; Name: "{localappdata}\Annex"
Type: filesandordirs; Name: "{userappdata}\Annex"
; Leftovers in the install folder (e.g. crash dumps, files written after install).
Type: filesandordirs; Name: "{app}"

[Code]
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
