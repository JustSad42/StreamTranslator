; StreamTranslator installer (Inno Setup 6). Built by build.ps1, which passes /DAppVersion=x.y.z
; Installs per user (no admin prompt) to %LOCALAPPDATA%\Programs\StreamTranslator.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "StreamTranslator"
#define AppExe "StreamTranslator.exe"

[Setup]
AppId={{4C7E1B92-3A5D-4F8E-B0C6-57A1D9E2F310}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppName}
AppComments=Translate streamers' VODs and live streams to English with Whisper
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
OutputDir=dist
OutputBaseFilename=StreamTranslator-Setup-{#AppVersion}
SetupIconFile=assets\icon.ico
WizardImageFile=assets\wizard-side.bmp,assets\wizard-side@2x.bmp
WizardSmallImageFile=assets\wizard-small.bmp,assets\wizard-small@2x.bmp
WizardStyle=modern
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
; the CUDA libraries are huge; fast compression keeps the build quick
Compression=lzma2/fast
SolidCompression=no
LZMAUseSeparateProcess=yes
LZMANumBlockThreads=8
DiskSpanning=no
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
WelcomeLabel2=This will install [name/ver] on your computer.%n%nTranslate other streamers' VODs and live streams into English with Whisper running on your own NVIDIA GPU.%n%nNo administrator rights are needed. You also need FFmpeg (winget install Gyan.FFmpeg).
FinishedLabel=[name] is installed. Find it in the Start menu. The first translation downloads the Whisper model (about 3 GB) once.

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "Extras:"; Flags: unchecked

[Files]
Source: "dist\StreamTranslator\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[InstallDelete]
Type: filesandordirs; Name: "{app}\_internal"

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; Comment: "Translate streamers into English"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; downloaded yt-dlp updates and models. Settings, history and glossary in %APPDATA%\StreamTranslator are kept.
Type: filesandordirs; Name: "{localappdata}\StreamTranslator\pylib"
Type: filesandordirs; Name: "{localappdata}\StreamTranslator\models"
