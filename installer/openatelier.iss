; The Windows installer (Inno Setup 6): OpenAtelier-<version>-windows-x64-setup.exe.
;
; Built by .github/workflows/release.yml from the packaged folder:
;   iscc /DAppVersion=0.1.0-beta.1 /DSourceDir=dist\OpenAtelier-0.1.0-beta.1-windows-x64 /DOutputDir=dist installer\openatelier.iss
;
; It installs for the current user, into %LOCALAPPDATA%\Programs\OpenAtelier — no
; administrator prompt, and the app can update itself there (crates/app/src/update.rs).
; Installing for everyone (Program Files) is offered too; the app then points to the
; download page for updates, since it can't write there.

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef SourceDir
  #error Pass /DSourceDir=<the packaged folder>
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
; The same for every version: Windows sees a new version as an upgrade of this one.
AppId={{B9F0B016-A9AE-4522-A4C0-80E8CCCF166D}
AppName=OpenAtelier
AppVersion={#AppVersion}
AppVerName=OpenAtelier {#AppVersion}
AppPublisher=OpenAtelier contributors
AppPublisherURL=https://github.com/CNK12321/OpenAtelier
AppSupportURL=https://github.com/CNK12321/OpenAtelier/issues
AppUpdatesURL=https://github.com/CNK12321/OpenAtelier/releases
DefaultDirName={autopf}\OpenAtelier
DefaultGroupName=OpenAtelier
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
LicenseFile=..\LICENSE
OutputDir={#OutputDir}
OutputBaseFilename=OpenAtelier-{#AppVersion}-windows-x64-setup
SetupIconFile=..\assets\logo.ico
UninstallDisplayIcon={app}\OpenAtelier.exe
UninstallDisplayName=OpenAtelier
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Closes a running OpenAtelier before replacing it (asking first).
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\OpenAtelier"; Filename: "{app}\OpenAtelier.exe"
Name: "{autodesktop}\OpenAtelier"; Filename: "{app}\OpenAtelier.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\OpenAtelier.exe"; Description: "{cm:LaunchProgram,OpenAtelier}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; What the in-app updater set aside (files it replaced while they were in use).
Type: files; Name: "{app}\*.old"
Type: files; Name: "{app}\*.old?"
