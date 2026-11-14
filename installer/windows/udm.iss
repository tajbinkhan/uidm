; Inno Setup script for UDM's Windows installer. It installs for the current
; user only, needs no administrator rights, and does what install-host.ps1
; does: the three programs, the extension folder and its Firefox copy, the
; native-messaging host registered with Chrome, Edge and Firefox, and a Start
; menu shortcut.
;
; Build it with installer\windows\build.ps1, which builds the programs and passes the
; version from Cargo.toml. To call the compiler yourself:
;   ISCC.exe /DAppVersion=0.3.0 installer\windows\udm.iss

#ifndef AppVersion
  #define AppVersion "0.3.0"
#endif
#define HostName "com.udm.host"
; The unpacked extension's ID, fixed by the public key in
; extension\chromium\manifest.json.
#define ExtensionId "epmjakacaejhdhldkpbjnakhfmjjbhfm"
; Every Chromium extension ID the host lets in, separated by spaces: the
; unpacked one, then the ones the stores give their copies. To add the
; Chrome Web Store's, write: #define ChromiumIds ExtensionId + " <its ID>"
; It must match CHROMIUM_EXTENSION_IDS in crates\infrastructure\src\browsers.rs,
; which a test there checks (docs\extension-stores.md).
#define ChromiumIds ExtensionId
; From extension\firefox.json.
#define FirefoxId "udm@udm.local"
#define Root "..\.."

[Setup]
AppId={{8C6F2B1E-5D3A-4E7B-9A41-2F0C7D9E6B13}
AppName=UDM
AppVersion={#AppVersion}
AppVerName=UDM {#AppVersion}
AppPublisher=Tajbin Khan
AppPublisherURL=https://github.com/tajbinkhan/UDM
AppSupportURL=https://github.com/tajbinkhan/UDM/issues
VersionInfoVersion={#AppVersion}
VersionInfoDescription=UDM setup
; Per user: the browsers read native-messaging hosts from HKCU, and the
; daemon keeps its data under the user's profile. {autopf} is then
; %LOCALAPPDATA%\Programs.
PrivilegesRequired=lowest
#if Ver >= EncodeVer(6, 7, 0)
; Inno Setup 6.7 turns on Windows' RedirectionGuard for Setup, and every
; program Setup starts inherits it: UDM from the last page or after Update
; now, then its daemon, then yt-dlp. yt-dlp then fails with WinError 448 on
; a junction a user made, such as pnpm's on PATH. The guard stops junction
; tricks that gain privileges, and this Setup never has more than the user.
RedirectionGuard=no
#endif
DefaultDirName={autopf}\UDM
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#Root}\dist
OutputBaseFilename=UDM-Setup-{#AppVersion}
SetupIconFile=udm.ico
UninstallDisplayIcon={app}\udm.ico
UninstallDisplayName=UDM
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
; Running UDM programs are closed in [Code], after asking.
CloseApplications=no

[Tasks]
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
; Shown only when winget is there and yt-dlp is not. winget installs yt-dlp
; with FFmpeg and Deno, which it depends on.
Name: videotools; Description: "Install yt-dlp, FFmpeg and Deno with winget, for YouTube and other video sites (downloads them now)"; GroupDescription: "Video tools:"; Check: CanInstallVideoTools

[Files]
Source: "{#Root}\target\release\udm-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\target\release\udm-daemon.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\target\release\udm-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\target\release\udm-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\target\release\udm-mcp.exe"; DestDir: "{app}"; Flags: ignoreversion
; A windows-gnu build of udm-gui loads the WebView2 loader from this DLL,
; which tauri-build copies beside it; an MSVC build links the loader in and
; has no DLL to ship.
Source: "{#Root}\target\release\WebView2Loader.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
; Installed as extension and firefox, as before the repository split them,
; so a browser that loaded one keeps finding it after an update. build.ps1
; checks that the Firefox copy is current.
Source: "{#Root}\extension\chromium\*"; DestDir: "{app}\extension"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Root}\extension\firefox\*"; DestDir: "{app}\firefox"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "udm.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
; Microsoft's WebView2 bootstrapper, which build.ps1 downloads: the window
; draws with the WebView2 runtime, installed here only when it is missing.
Source: "MicrosoftEdgeWebview2Setup.exe"; DestDir: "{tmp}"; Flags: deleteafterinstall; Check: NeedsWebView2

[Registry]
; The browsers find udm-host.exe through the manifest these keys name. The
; manifest itself is written in [Code], with this install's path in it.
Root: HKCU; Subkey: "Software\Google\Chrome\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.json"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Microsoft\Edge\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.json"; Flags: uninsdeletekey
; Firefox names the allowed extension by ID, so it has its own manifest.
Root: HKCU; Subkey: "Software\Mozilla\NativeMessagingHosts\{#HostName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#HostName}.firefox.json"; Flags: uninsdeletekey
; Options can register other browsers; uninstalling removes those too.
Root: HKCU; Subkey: "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\{#HostName}"; Flags: uninsdeletekey dontcreatekey
Root: HKCU; Subkey: "Software\Chromium\NativeMessagingHosts\{#HostName}"; Flags: uninsdeletekey dontcreatekey

[Icons]
Name: "{autoprograms}\UDM"; Filename: "{app}\udm-gui.exe"; WorkingDir: "{app}"; IconFilename: "{app}\udm.ico"; Comment: "UDM download manager"
Name: "{autodesktop}\UDM"; Filename: "{app}\udm-gui.exe"; WorkingDir: "{app}"; IconFilename: "{app}\udm.ico"; Comment: "UDM download manager"; Tasks: desktopicon

[Run]
Filename: "{tmp}\MicrosoftEdgeWebview2Setup.exe"; Parameters: "/silent /install"; StatusMsg: "Installing the Microsoft Edge WebView2 runtime..."; Flags: waituntilterminated; Check: NeedsWebView2
Filename: "{app}\udm-gui.exe"; Description: "{cm:LaunchProgram,UDM}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\extension"; Description: "Open the extension folder, to load it in the browser"; Flags: shellexec nowait postinstall skipifsilent unchecked; Check: LoadsByHand

[InstallDelete]
; The shortcuts of UDM's earlier names, Pullbay (0.4) and UIDM (up to 0.3.3).
Type: files; Name: "{autoprograms}\Pullbay.lnk"
Type: files; Name: "{autodesktop}\Pullbay.lnk"
Type: files; Name: "{autoprograms}\UIDM.lnk"
Type: files; Name: "{autodesktop}\UIDM.lnk"

[UninstallDelete]
Type: files; Name: "{app}\{#HostName}.json"
Type: files; Name: "{app}\{#HostName}.firefox.json"
Type: files; Name: "{localappdata}\UDM\{#HostName}.json"
Type: files; Name: "{localappdata}\UDM\{#HostName}.firefox.json"

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  { Written by the daemon so Windows shows UDM's notifications. }
  NotificationKey = 'Software\Classes\AppUserModelId\UDM.DownloadManager';
  { The extension's pages in the Chrome Web Store and on Firefox Add-ons,
    once it is listed there (docs\extension-stores.md). While one is empty,
    the last page says how to load that copy by hand. }
  ChromeStorePage = '';
  FirefoxStorePage = '';

var
  { The folder Pullbay or UIDM, as UDM was called in 0.4 and up to 0.3.3,
    was installed in, when this Setup updates it; empty otherwise. }
  OldDir: String;
  { That earlier name, Pullbay or UIDM; empty otherwise. }
  OldName: String;

{ An update of Pullbay or UIDM installs into a folder named UDM beside the
  old one, which goes once its programs are closed. UDM's own updates keep
  the folder they were in. }
procedure InitializeWizard();
var
  Dir, Name: String;
begin
  Dir := RemoveBackslash(WizardForm.DirEdit.Text);
  Name := ExtractFileName(Dir);
  if (CompareText(Name, 'Pullbay') = 0) or (CompareText(Name, 'UIDM') = 0) then
  begin
    OldDir := Dir;
    if CompareText(Name, 'UIDM') = 0 then
      OldName := 'UIDM'
    else
      OldName := 'Pullbay';
    WizardForm.DirEdit.Text := AddBackslash(ExtractFileDir(Dir)) + 'UDM';
  end;
end;

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

{ The Chromium host manifest's allowed origins: one chrome-extension://
  origin for each ID in ChromiumIds. }
function ChromiumOrigins(): String;
var
  Ids: String;
  Space: Integer;
begin
  Result := '';
  Ids := Trim('{#ChromiumIds}');
  while Ids <> '' do
  begin
    Space := Pos(' ', Ids);
    if Space = 0 then
      Space := Length(Ids) + 1;
    if Result <> '' then
      Result := Result + ', ';
    Result := Result + '"chrome-extension://' + Copy(Ids, 1, Space - 1) + '/"';
    Ids := Trim(Copy(Ids, Space + 1, Length(Ids)));
  end;
end;

{ Whether the Chromium extension is still loaded by hand: it has no page in
  the Chrome Web Store yet. }
function LoadsByHand(): Boolean;
begin
  Result := ChromeStorePage = '';
end;

{ Whether the manifest at Path names this install's udm-host.exe, as the
  ones UDM writes in its data folder when Options registers Brave or
  Chromium do. A folder name outside ASCII, which UDM writes as is, does not
  match, and such a manifest is left as it is. }
function NamesThisHost(Path: String): Boolean;
var
  Raw: AnsiString;
  Json: String;
begin
  Result := False;
  if not LoadStringFromFile(Path, Raw) then
    exit;
  Json := Raw;
  Result := Pos(Lowercase(JsonString(ExpandConstant('{app}\udm-host.exe'))), Lowercase(Json)) > 0;
end;

{ A native-messaging manifest in Dir: where udm-host.exe is, and the
  extensions allowed to start it. Allowed is the last line, such as
  "allowed_origins": [...]. }
procedure WriteHostManifest(Dir, FileName, Allowed: String);
var
  Json, Path: String;
begin
  Json :=
    '{' + #13#10 +
    '  "name": "{#HostName}",' + #13#10 +
    '  "description": "UDM download manager native-messaging host",' + #13#10 +
    '  "type": "stdio",' + #13#10 +
    '  "path": ' + JsonString(ExpandConstant('{app}\udm-host.exe')) + ',' + #13#10 +
    '  ' + Allowed + #13#10 +
    '}' + #13#10;
  Path := AddBackslash(Dir) + FileName;
  if not SaveStringToFile(Path, Json, False) then
    RaiseException('Could not write ' + Path);
end;

{ Find the UDM programs running from this install's folder, and Pullbay's
  or UIDM's from the folder this Setup updates, and with Stop end them. True when any
  were running. Programs of the same name elsewhere (a development build)
  are left alone. }
function UdmRunning(Stop: Boolean): Boolean;
var
  Dir, Old, Script: String;
  Code: Integer;
begin
  Dir := AddBackslash(ExpandConstant('{app}'));
  StringChangeEx(Dir, '''', '''''', True);
  Old := '';
  if OldDir <> '' then
    Old := AddBackslash(OldDir);
  StringChangeEx(Old, '''', '''''', True);
  Script :=
    '$d = ''' + Dir + '''; $o = ''' + Old + '''; ' +
    '$p = @(Get-Process udm-gui,udm-daemon,udm-host,udm-cli,udm-mcp,pullbay-gui,pullbay-daemon,pullbay-host,uidm-gui,uidm-daemon,uidm-host -ErrorAction SilentlyContinue | ' +
    'Where-Object { $_.Path -and ($_.Path.StartsWith($d, [StringComparison]::OrdinalIgnoreCase) -or ' +
    '($o -and $_.Path.StartsWith($o, [StringComparison]::OrdinalIgnoreCase))) }); ' +
    'if ($p.Count -eq 0) { exit 0 }; ';
  if Stop then
    Script := Script + '$p | ForEach-Object { $_.Kill(); [void]$_.WaitForExit(5000) }; ';
  Script := Script + 'exit 1';
  Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    '-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "' + Script + '"',
    '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := Code = 1;
end;

{ Close a running UDM after asking. False when the user says no. }
function CloseUdm(Silent: Boolean): Boolean;
begin
  Result := True;
  if not UdmRunning(False) then
    exit;
  if not Silent then
    Result := MsgBox('UDM is running. It will be closed first.' + #13#10#13#10 +
      'Downloads in progress stop and can be resumed from UDM''s list.',
      mbConfirmation, MB_OKCANCEL) = IDOK;
  if Result then
    UdmRunning(True);
end;

{ Started by UDM's Update now (/fromudm=yes), Pullbay's (/frompullbay=yes)
  or UIDM's (/fromuidm=yes): the user already agreed to close it. }
function FromUdm(): Boolean;
begin
  Result := (ExpandConstant('{param:fromudm|no}') = 'yes') or
    (ExpandConstant('{param:frompullbay|no}') = 'yes') or
    (ExpandConstant('{param:fromuidm|no}') = 'yes');
end;

function SetEnvironmentVariable(Name, Value: String): BOOL;
  external 'SetEnvironmentVariableW@kernel32.dll stdcall';
function ExpandEnvironmentStrings(Src: String; Dst: String; Size: DWORD): DWORD;
  external 'ExpandEnvironmentStringsW@kernel32.dll stdcall';

{ winget, through the alias Windows 10 1809 and later put in WindowsApps. }
const
  WebView2Key = 'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';

{ Whether WebView2 is installed under Root: its version, pv, is set and not
  0.0.0.0, as Microsoft's distribution guide checks it. }
function WebView2In(Root: Integer): Boolean;
var
  Version: String;
begin
  Result := RegQueryStringValue(Root, WebView2Key, 'pv', Version) and (Version <> '') and (Version <> '0.0.0.0');
end;

{ The window draws with the WebView2 runtime. Windows 11 has it, and
  Windows 10 gets it with Edge; Setup installs it for this user when neither
  the computer nor the user has it. }
function NeedsWebView2(): Boolean;
begin
  Result := not (WebView2In(HKLM32) or WebView2In(HKCU));
end;

function WingetPath(): String;
begin
  Result := ExpandConstant('{localappdata}\Microsoft\WindowsApps\winget.exe');
end;

function YtDlpFound(): Boolean;
begin
  Result := FileSearch('yt-dlp.exe', GetEnv('PATH')) <> '';
end;

{ The video tools task is offered when winget can install yt-dlp and it is
  not installed yet. }
function CanInstallVideoTools(): Boolean;
begin
  Result := FileExists(WingetPath()) and not YtDlpFound();
end;

{ %VARS% in S filled in. }
function Expand(S: String): String;
var
  Size: DWORD;
begin
  Result := S;
  if Pos('%', S) = 0 then
    exit;
  Size := ExpandEnvironmentStrings(S, '', 0);
  if Size = 0 then
    exit;
  SetLength(Result, Size);
  if ExpandEnvironmentStrings(S, Result, Size) = 0 then
    Result := S
  else
    SetLength(Result, Size - 1);
end;

{ winget puts the tools on the user's PATH in the registry. Setup's own PATH
  is the one it started with, and UDM started from the last page inherits
  it, so read the new one in. }
procedure RefreshPath();
var
  Machine, User, Path: String;
begin
  RegQueryStringValue(HKLM, 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', 'Path', Machine);
  RegQueryStringValue(HKCU, 'Environment', 'Path', User);
  Path := Expand(Machine);
  if User <> '' then
  begin
    if (Path <> '') and (Path[Length(Path)] <> ';') then
      Path := Path + ';';
    Path := Path + Expand(User);
  end;
  if Path <> '' then
    SetEnvironmentVariable('PATH', Path);
end;

procedure InstallVideoTools();
var
  Code: Integer;
  Ok: Boolean;
begin
  WizardForm.StatusLabel.Caption := 'Installing yt-dlp, FFmpeg and Deno with winget. This can take a few minutes...';
  WizardForm.FilenameLabel.Caption := '';
  WizardForm.ProgressGauge.Style := npbstMarquee;
  try
    Ok := Exec(WingetPath(),
      'install --id yt-dlp.yt-dlp --exact --source winget --silent --disable-interactivity ' +
      '--accept-package-agreements --accept-source-agreements',
      '', SW_HIDE, ewWaitUntilTerminated, Code);
  finally
    WizardForm.ProgressGauge.Style := npbstNormal;
  end;
  RefreshPath();
  if not (Ok and YtDlpFound()) then
    SuppressibleMsgBox('winget could not install the video tools (exit code ' + IntToStr(Code) + ').' + #13#10#13#10 +
      'UDM still downloads ordinary files. For YouTube and other video sites, use Install yt-dlp ' +
      'and Install ffmpeg in UDM''s Options > Tools, or run this later:' + #13#10 +
      '    winget install yt-dlp.yt-dlp',
      mbError, MB_OK, IDOK);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not CloseUdm(WizardSilent or FromUdm()) then
    Result := 'UDM is still running. Close it and run Setup again.';
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Data: String;
begin
  if CurStep = ssPostInstall then
  begin
    WriteHostManifest(ExpandConstant('{app}'), '{#HostName}.json', '"allowed_origins": [' + ChromiumOrigins() + ']');
    WriteHostManifest(ExpandConstant('{app}'), '{#HostName}.firefox.json', '"allowed_extensions": ["{#FirefoxId}"]');
    { Browsers registered from Options (Brave, Chromium) read the manifests
      in UDM's data folder, which UDM itself does not write again on
      Windows: bring the ones for this install up to date, so they let in
      the extensions this version does. }
    Data := ExpandConstant('{localappdata}\UDM');
    if NamesThisHost(Data + '\{#HostName}.json') then
      WriteHostManifest(Data, '{#HostName}.json', '"allowed_origins": [' + ChromiumOrigins() + ']');
    if NamesThisHost(Data + '\{#HostName}.firefox.json') then
      WriteHostManifest(Data, '{#HostName}.firefox.json', '"allowed_extensions": ["{#FirefoxId}"]');
    { The old name's programs are closed by now. Its settings, list,
      browser registrations and sign-in entry move when UDM first starts. }
    if (OldDir <> '') and DirExists(OldDir) then
      DelTree(OldDir, True, True, True);
    if WizardIsTaskSelected('videotools') then
      InstallVideoTools();
  end;
end;

procedure CurPageChanged(CurPageID: Integer);
var
  Video, Chromium, Firefox: String;
begin
  if CurPageID <> wpFinished then
    exit;
  if YtDlpFound() then
    Video := ''
  else
    Video := #13#10#13#10 +
      'For YouTube and other video sites, use Install yt-dlp and Install ffmpeg in UDM''s ' +
      'Options > Tools, or run: winget install yt-dlp.yt-dlp';
  if OldDir <> '' then
    Video := #13#10#13#10 +
      OldName + ' is now called UDM. Remove the ' + OldName + ' extension from the browser first: ' +
      'it went with ' + OldName + '''s folder, and UDM''s takes its place.' + Video;
  if ChromeStorePage <> '' then
    Chromium :=
      'UDM is installed. To catch downloads in Chrome or Edge, add its extension from the Chrome Web Store ' +
      '(Edge asks you to allow extensions from other stores first):' + #13#10 +
      '    ' + ChromeStorePage
  else
    Chromium :=
      'UDM is installed. To catch downloads in Chrome or Edge, load its extension once:' + #13#10#13#10 +
      '1. Open chrome://extensions (or edge://extensions) and turn on Developer mode.' + #13#10 +
      '2. Click Load unpacked and choose this folder:' + #13#10 +
      '    ' + ExpandConstant('{app}\extension') + #13#10 +
      '3. Check that its ID reads {#ExtensionId}.';
  if FirefoxStorePage <> '' then
    Firefox := 'In Firefox, add it from Firefox Add-ons: ' + FirefoxStorePage
  else
    Firefox :=
      'In Firefox, open about:debugging, choose This Firefox, then Load Temporary Add-on, ' +
      'and pick manifest.json in ' + ExpandConstant('{app}\firefox') + '.';
  WizardForm.FinishedLabel.Caption := Chromium + #13#10#13#10 + Firefox + Video;
end;

function InitializeUninstall(): Boolean;
begin
  Result := CloseUdm(UninstallSilent);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Command: String;
begin
  if CurUninstallStep <> usPostUninstall then
    exit;
  RegDeleteKeyIncludingSubkeys(HKCU, NotificationKey);
  { "Start with Windows", when it points at this install. }
  if RegQueryStringValue(HKCU, RunKey, 'UDM', Command) and
     (Pos(Lowercase(ExpandConstant('{app}')), Lowercase(Command)) > 0) then
    RegDeleteValue(HKCU, RunKey, 'UDM');
  if not UninstallSilent and
     (MsgBox('Also remove your UDM settings and download list?' + #13#10#13#10 +
       'Downloaded files are kept either way.',
       mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES) then
  begin
    DelTree(ExpandConstant('{userappdata}\UDM'), True, True, True);
    DelTree(ExpandConstant('{localappdata}\UDM'), True, True, True);
  end;
end;
