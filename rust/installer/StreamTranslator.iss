; StreamTranslator 3 (Rust) installer, Inno Setup 6. Built by ..\build-installer.ps1, which stages the files in
; ..\dist\StreamTranslator and passes /DAppVersion=x.y.z.
;
; Same AppId, install folder, mutex and file name as the Python app's installer (..\..\installer.iss), so the
; installed 2.x apps update to this in place with their own updater (/SILENT /SUPPRESSMSGBOXES /NORESTART /LOG=...).
; Installs per user (no admin prompt) to %LOCALAPPDATA%\Programs\StreamTranslator.
;
; /DTestBuild=1 makes a separate test app (own AppId and folder, keeps downloaded components on uninstall), for
; trying install/uninstall without touching a real installation.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "StreamTranslator"
#define AppExe "StreamTranslator.exe"
; Windows uses this to put the app's name and icon on its notifications (must match tauri.conf.json "identifier")
#define AppUserModelID "com.justsad42.streamtranslator"

#ifdef TestBuild
  #define TheAppId "{{9D3A51C4-7B2E-4F0A-8C61-2E5B7A9F0D13}"
  #define InstallName "StreamTranslator Test"
#else
  #define TheAppId "{{4C7E1B92-3A5D-4F8E-B0C6-57A1D9E2F310}"
  #define InstallName "StreamTranslator"
#endif

[Setup]
AppId={#TheAppId}
AppName={#InstallName}
AppVersion={#AppVersion}
AppVerName={#InstallName} {#AppVersion}
AppPublisher={#AppName}
AppComments=Translate streamers' VODs and live streams to English with Whisper
DefaultDirName={autopf}\{#InstallName}
DefaultGroupName={#InstallName}
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
OutputDir=..\dist
#ifdef TestBuild
OutputBaseFilename=StreamTranslator-Test-Setup-{#AppVersion}
#else
OutputBaseFilename=StreamTranslator-Setup-{#AppVersion}
#endif
SetupIconFile=..\..\assets\icon.ico
WizardImageFile=..\..\assets\wizard-side.bmp,..\..\assets\wizard-side@2x.bmp
WizardSmallImageFile=..\..\assets\wizard-small.bmp,..\..\assets\wizard-small@2x.bmp
WizardStyle=modern
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#InstallName}
Compression=lzma2/max
SolidCompression=yes
LZMAUseSeparateProcess=yes
LZMANumBlockThreads=8
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
AppMutex=StreamTranslatorAppMutex
CloseApplications=yes
RestartApplications=no
VersionInfoVersion={#AppVersion}
VersionInfoProductName={#AppName}
VersionInfoDescription={#AppName} Setup

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Messages]
WelcomeLabel2=This will install [name/ver] on your computer.%n%nTranslate other streamers' VODs and live streams into English with Whisper running on your own NVIDIA GPU.%n%nNo administrator rights are needed. On first run, a setup screen downloads the GPU libraries and models it needs (reusing anything already on this PC).
FinishedLabel=[name] is installed. Find it in the Start menu. The first time it opens, a setup screen gets the GPU libraries and models ready.

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "Extras:"; Flags: unchecked

[Files]
Source: "..\dist\StreamTranslator\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[InstallDelete]
; the Python versions (2.x) kept everything in _internal
Type: filesandordirs; Name: "{app}\_internal"

[Icons]
Name: "{autoprograms}\{#InstallName}"; Filename: "{app}\{#AppExe}"; Comment: "Translate streamers into English"; AppUserModelID: "{#AppUserModelID}"
Name: "{autodesktop}\{#InstallName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon; AppUserModelID: "{#AppUserModelID}"

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#InstallName}}"; Flags: nowait postinstall skipifsilent
; in-app updates run this installer with /SILENT: reopen the app when done
Filename: "{app}\{#AppExe}"; Flags: nowait; Check: WizardSilent

#ifndef TestBuild
[UninstallDelete]
; downloaded components. Settings, history and glossary in %APPDATA%\StreamTranslator are kept.
Type: filesandordirs; Name: "{localappdata}\StreamTranslator\pylib"
Type: filesandordirs; Name: "{localappdata}\StreamTranslator\yt-dlp"
Type: filesandordirs; Name: "{localappdata}\StreamTranslator\models"
Type: filesandordirs; Name: "{localappdata}\StreamTranslator\cuda"
Type: filesandordirs; Name: "{localappdata}\StreamTranslator\icons"
Type: filesandordirs; Name: "{localappdata}\StreamTranslator\updates"
#endif

[Code]
// The window is Microsoft Edge WebView2. It comes with Windows 11 and current Windows 10; on a PC without it,
// fetch Microsoft's small online installer and run it (per user, no admin needed).
const
  WebView2Key = 'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';
  WebView2Url = 'https://go.microsoft.com/fwlink/p/?LinkId=2124703';

function WebView2Installed(): Boolean;
var
  v: String;
begin
  Result := (RegQueryStringValue(HKLM, 'SOFTWARE\WOW6432Node\' + Copy(WebView2Key, 10, 999), 'pv', v) and (v <> '') and (v <> '0.0.0.0'))
         or (RegQueryStringValue(HKCU, WebView2Key, 'pv', v) and (v <> '') and (v <> '0.0.0.0'));
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  code: Integer;
begin
  Result := '';
  if WebView2Installed() then
    exit;
  try
    DownloadTemporaryFile(WebView2Url, 'MicrosoftEdgeWebview2Setup.exe', '', nil);
    if not Exec(ExpandConstant('{tmp}\MicrosoftEdgeWebview2Setup.exe'), '/silent /install', '', SW_HIDE, ewWaitUntilTerminated, code) or not WebView2Installed() then
      Log('WebView2 setup did not finish (exit code ' + IntToStr(code) + ')');
  except
    Log('Could not download WebView2: ' + GetExceptionMessage);
  end;
  if not WebView2Installed() then
    Result := 'StreamTranslator needs Microsoft Edge WebView2, and it could not be installed automatically.' + #13#10 +
              'Install it from https://developer.microsoft.com/microsoft-edge/webview2 and run this setup again.';
end;
