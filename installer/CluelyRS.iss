; Inno Setup script for CluelyRS. Build with dev\build-installer.ps1.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef AppExe
  #define AppExe "..\target\release\cluely-rs.exe"
#endif
#ifndef ThirdPartyLicenses
  #define ThirdPartyLicenses "..\target\THIRD_PARTY_LICENSES.txt"
#endif

[Setup]
; Fixed identity for upgrades. Never change this GUID.
AppId={{9F4A1F7F-5E10-4E2D-AC7B-D56244540794}
AppName=CluelyRS
AppVersion={#AppVersion}
AppPublisher=Jay Martinez
AppPublisherURL=https://github.com/jayamartinez/cluely-rs
AppSupportURL=https://github.com/jayamartinez/cluely-rs
AppUpdatesURL=https://github.com/jayamartinez/cluely-rs
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\CluelyRS
DisableDirPage=yes
DisableProgramGroupPage=yes
UsePreviousAppDir=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
AppMutex=Local\CluelyRS.SingleInstance
CloseApplications=yes
SetupIconFile=..\assets\app-icon\app-icon.ico
UninstallDisplayIcon={app}\CluelyRS.exe
WizardStyle=modern
; One click: no pages to step through, just the progress page, then CluelyRS starts.
DisableWelcomePage=yes
DisableReadyPage=yes
DisableFinishedPage=yes
; The progress page's header shows CluelyRS's icon (rendered from assets\app-icon) instead of
; Inno's stock box, at 100/150/200% scale; Setup picks the closest to the display's DPI.
WizardSmallImageFile=wizard-small-100.png,wizard-small-150.png,wizard-small-200.png
; "CluelyRS" rather than the default "CluelyRS version 0.1.0" in Installed apps.
UninstallDisplayName=CluelyRS
Compression=lzma2/max
SolidCompression=yes
OutputDir=..\target\installer
OutputBaseFilename=CluelyRS-{#AppVersion}-setup

[Messages]
SetupWindowTitle=CluelyRS Setup
UninstallAppFullTitle=Uninstall CluelyRS

[Files]
Source: "{#AppExe}"; DestDir: "{app}"; DestName: "CluelyRS.exe"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "{#ThirdPartyLicenses}"; DestDir: "{app}"; DestName: "THIRD_PARTY_LICENSES.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\CluelyRS"; Filename: "{app}\CluelyRS.exe"
[Run]
Filename: "{app}\CluelyRS.exe"; Flags: nowait skipifsilent

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  // Deletes the API keys: generic credentials whose target ends in ".CluelyRS" (the keyring crate
  // names them <user>.<service>). cmdkey lists them as "LegacyGeneric:target=<name>" but deletes
  // them by the bare name. No double quotes, as the whole script is passed in them.
  CredCleanupScript =
    'foreach ($l in (cmdkey /list)) { if ($l -match ''target=(.+\.CluelyRS)\s*$'') { cmdkey (''/delete:'' + $matches[1]) | Out-Null } }';

// With every other page disabled, Setup still shows the Ready page as a confirmation (skipping it
// isn't allowed), so press Install for the user: posted, so it runs once the page has finished
// showing, and the install starts as soon as Setup opens.
procedure CurPageChanged(CurPageID: Integer);
begin
  if CurPageID = wpReady then
    PostMessage(WizardForm.NextButton.Handle, $00F5 { BM_CLICK }, 0, 0);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
  PsExe: String;
begin
  if CurUninstallStep = usUninstall then
    // Remove autostart entry if present. Install never creates it.
    RegDeleteValue(HKEY_CURRENT_USER, RunKey, 'CluelyRS');

  if CurUninstallStep = usPostUninstall then
  begin
    if UninstallSilent then
      exit;
    if MsgBox('Also delete your CluelyRS settings, sessions and downloaded speech models? ' +
              'API keys saved in Windows Credential Manager are removed too.',
              mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(ExpandConstant('{userappdata}\CluelyRS'), True, True, True);
      DelTree(ExpandConstant('{localappdata}\CluelyRS'), True, True, True);
      PsExe := ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe');
      Exec(PsExe, '-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -Command "' +
           CredCleanupScript + '"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    end;
  end;
end;
