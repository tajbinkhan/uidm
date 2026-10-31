; Inno Setup script for UIDM's Windows installer. It installs for the current
; user only, needs no administrator rights, and does what install-host.ps1
; does: the three programs, the extension folder and its Firefox copy, the
; native-messaging host registered with Chrome, Edge and Firefox, and a Start
; menu shortcut.
;
; Build it with installer\build.ps1, which builds the programs and passes the
; version from Cargo.toml. To call the compiler yourself:
;   ISCC.exe /DAppVersion=0.1.0 installer\uidm.iss

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#define HostName "com.uidm.host"
; Fixed by the public key in extension\manifest.json.
#define ExtensionId "epmjakacaejhdhldkpbjnakhfmjjbhfm"
; From extension\firefox.json.
#define FirefoxId "uidm@uidm.local"
#define Root ".."

[Setup]
AppId={{8C6F2B1E-5D3A-4E7B-9A41-2F0C7D9E6B13}
AppName=UIDM
AppVersion={#AppVersion}
AppVerName=UIDM {#AppVersion}
AppPublisher=Tajbin Khan
AppPublisherURL=https://github.com/tajbinkhan/uidm
AppSupportURL=https://github.com/tajbinkhan/uidm/issues
VersionInfoVersion={#AppVersion}
VersionInfoDescription=UIDM setup
; Per user: the browsers read native-messaging hosts from HKCU, and the
; daemon keeps its data under the user's profile. {autopf} is then
; %LOCALAPPDATA%\Programs.
PrivilegesRequired=lowest
DefaultDirName={autopf}\UIDM
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#Root}\dist
OutputBaseFilename=UIDM-Setup-{#AppVersion}
SetupIconFile=uidm.ico
UninstallDisplayIcon={app}\uidm.ico
UninstallDisplayName=UIDM
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
; Running UIDM programs are closed in [Code], after asking.
CloseApplications=no

[Tasks]
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Root}\target\release\uidm-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\target\release\uidm-daemon.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\target\release\uidm-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\extension\*"; DestDir: "{app}\extension"; Excludes: "test\*,firefox.json"; Flags: ignoreversion recursesubdirs createallsubdirs
; Made by build.ps1 with make-firefox.ps1.
Source: "{#Root}\firefox\*"; DestDir: "{app}\firefox"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "uidm.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Registry]
; The browsers find uidm-host.exe through the manifest these keys name. The
; manifest itself is written in [Code], with this install's path in it.
Root: HKCU; Subkey: "Software\Google\Chrome\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.json"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Microsoft\Edge\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.json"; Flags: uninsdeletekey
; Firefox names the allowed extension by ID, so it has its own manifest.
Root: HKCU; Subkey: "Software\Mozilla\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.firefox.json"; Flags: uninsdeletekey
; Options can register other browsers; uninstalling removes those too.
Root: HKCU; Subkey: "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\{#HostName}"; Flags: uninsdeletekey dontcreatekey
Root: HKCU; Subkey: "Software\Chromium\NativeMessagingHosts\{#HostName}"; Flags: uninsdeletekey dontcreatekey

[Icons]
Name: "{autoprograms}\UIDM"; Filename: "{app}\uidm-gui.exe"; WorkingDir: "{app}"; IconFilename: "{app}\uidm.ico"; Comment: "UIDM download manager"
Name: "{autodesktop}\UIDM"; Filename: "{app}\uidm-gui.exe"; WorkingDir: "{app}"; IconFilename: "{app}\uidm.ico"; Comment: "UIDM download manager"; Tasks: desktopicon

[Run]
Filename: "{app}\uidm-gui.exe"; Description: "{cm:LaunchProgram,UIDM}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\extension"; Description: "Open the extension folder, to load it in the browser"; Flags: shellexec nowait postinstall skipifsilent unchecked

[UninstallDelete]
Type: files; Name: "{app}\{#HostName}.json"
Type: files; Name: "{app}\{#HostName}.firefox.json"
Type: files; Name: "{localappdata}\UIDM\{#HostName}.json"
Type: files; Name: "{localappdata}\UIDM\{#HostName}.firefox.json"

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  { Written by the daemon so Windows shows UIDM's notifications. }
  NotificationKey = 'Software\Classes\AppUserModelId\UIDM.DownloadManager';

{ A JSON string literal. Characters outside ASCII become \u escapes, so the
  file is plain ASCII whatever the user's folder is called. }
function JsonString(S: String): String;
var
  I: Integer;
  C: Char;
begin
  Result := '"';
  for I := 1 to Length(S) do
  begin
    C := S[I];
    if C = '\' then
      Result := Result + '\\'
    else if C = '"' then
      Result := Result + '\"'
    else if (Ord(C) < 32) or (Ord(C) > 126) then
      Result := Result + Format('\u%.4x', [Ord(C)])
    else
      Result := Result + C;
  end;
  Result := Result + '"';
end;

{ A native-messaging manifest: where uidm-host.exe is, and the only
  extension allowed to start it. Allowed is the last line, such as
  "allowed_origins": [...]. }
procedure WriteHostManifest(FileName, Allowed: String);
var
  Json, Path: String;
begin
  Json :=
    '{' + #13#10 +
    '  "name": "{#HostName}",' + #13#10 +
    '  "description": "UIDM download manager native-messaging host",' + #13#10 +
    '  "type": "stdio",' + #13#10 +
    '  "path": ' + JsonString(ExpandConstant('{app}\uidm-host.exe')) + ',' + #13#10 +
    '  ' + Allowed + #13#10 +
    '}' + #13#10;
  Path := ExpandConstant('{app}\') + FileName;
  if not SaveStringToFile(Path, Json, False) then
    RaiseException('Could not write ' + Path);
end;

{ Find the UIDM programs running from this install's folder and, with Stop,
  end them. True when any were running. Programs of the same name elsewhere
  (a development build) are left alone. }
function UidmRunning(Stop: Boolean): Boolean;
var
  Dir, Script: String;
  Code: Integer;
begin
  Dir := AddBackslash(ExpandConstant('{app}'));
  StringChangeEx(Dir, '''', '''''', True);
  Script :=
    '$d = ''' + Dir + '''; ' +
    '$p = @(Get-Process uidm-gui,uidm-daemon,uidm-host -ErrorAction SilentlyContinue | ' +
    'Where-Object { $_.Path -and $_.Path.StartsWith($d, [StringComparison]::OrdinalIgnoreCase) }); ' +
    'if ($p.Count -eq 0) { exit 0 }; ';
  if Stop then
    Script := Script + '$p | ForEach-Object { $_.Kill(); [void]$_.WaitForExit(5000) }; ';
  Script := Script + 'exit 1';
  Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    '-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "' + Script + '"',
    '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := Code = 1;
end;

{ Close a running UIDM after asking. False when the user says no. }
function CloseUidm(Silent: Boolean): Boolean;
begin
  Result := True;
  if not UidmRunning(False) then
    exit;
  if not Silent then
    Result := MsgBox('UIDM is running. It will be closed first.' + #13#10#13#10 +
      'Downloads in progress stop and can be resumed from UIDM''s list.',
      mbConfirmation, MB_OKCANCEL) = IDOK;
  if Result then
    UidmRunning(True);
end;

{ Started by UIDM's Update now (/fromuidm=yes): the user already agreed to
  close it. }
function FromUidm(): Boolean;
begin
  Result := ExpandConstant('{param:fromuidm|no}') = 'yes';
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not CloseUidm(WizardSilent or FromUidm()) then
    Result := 'UIDM is still running. Close it and run Setup again.';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    WriteHostManifest('{#HostName}.json', '"allowed_origins": ["chrome-extension://{#ExtensionId}/"]');
    WriteHostManifest('{#HostName}.firefox.json', '"allowed_extensions": ["{#FirefoxId}"]');
  end;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if CurPageID = wpFinished then
    WizardForm.FinishedLabel.Caption :=
      'UIDM is installed. To catch downloads in Chrome or Edge, load its extension once:' + #13#10#13#10 +
      '1. Open chrome://extensions (or edge://extensions) and turn on Developer mode.' + #13#10 +
      '2. Click Load unpacked and choose this folder:' + #13#10 +
      '    ' + ExpandConstant('{app}\extension') + #13#10 +
      '3. Check that its ID reads {#ExtensionId}.' + #13#10#13#10 +
      'In Firefox, open about:debugging, choose This Firefox, then Load Temporary Add-on, ' +
      'and pick manifest.json in ' + ExpandConstant('{app}\firefox') + '.' + #13#10#13#10 +
      'For streamed videos to be saved as .mp4, install ffmpeg and put it on PATH, or choose it in Options. ' +
      'For YouTube videos, also install yt-dlp: winget install yt-dlp.yt-dlp';
end;

function InitializeUninstall(): Boolean;
begin
  Result := CloseUidm(UninstallSilent);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Command: String;
begin
  if CurUninstallStep <> usPostUninstall then
    exit;
  RegDeleteKeyIncludingSubkeys(HKCU, NotificationKey);
  { "Start with Windows", when it points at this install. }
  if RegQueryStringValue(HKCU, RunKey, 'UIDM', Command) and
     (Pos(Lowercase(ExpandConstant('{app}')), Lowercase(Command)) > 0) then
    RegDeleteValue(HKCU, RunKey, 'UIDM');
  if not UninstallSilent and
     (MsgBox('Also remove your UIDM settings and download list?' + #13#10#13#10 +
       'Downloaded files are kept either way.',
       mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES) then
  begin
    DelTree(ExpandConstant('{userappdata}\UIDM'), True, True, True);
    DelTree(ExpandConstant('{localappdata}\UIDM'), True, True, True);
  end;
end;
