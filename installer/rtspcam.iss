; Inno Setup script for RTSP Cam. Built by tools/installer/build.ps1, which passes
; /DAppVersion=<workspace version> and /DBinDir=<folder with the release binaries>.
;
; Installs to C:\Program Files\RtspCam (the Frame Server services can read it) and registers
; the media source with the DLL's own DllRegisterServer, so repair and uninstall stay clean.
; Admin rights are needed at install and uninstall time only; the app itself runs as the user.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef BinDir
  #define BinDir "..\target\release"
#endif

[Setup]
AppId={{B7E4C1A2-5D3F-4B88-9C21-7A1E6F0D4C35}
AppName=RTSP Cam
AppVersion={#AppVersion}
AppVerName=RTSP Cam {#AppVersion}
AppPublisher=RTSP Cam
DefaultDirName={autopf}\RtspCam
DefaultGroupName=RTSP Cam
DisableProgramGroupPage=yes
UninstallDisplayName=RTSP Cam
UninstallDisplayIcon={app}\rtspcam.exe
OutputDir=out
OutputBaseFilename=RtspCam-{#AppVersion}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=admin
; The optional autostart task writes HKCU on purpose (see the comment in [Registry]).
UsedUserAreasWarning=no
; Virtual cameras need Windows 11 (build 22000).
MinVersion=10.0.22000
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Close a running rtspcam.exe (Restart Manager) before replacing files, and again at uninstall.
CloseApplications=yes
RestartApplications=no
VersionInfoVersion={#AppVersion}
VersionInfoDescription=RTSP Cam setup
#ifdef SignTool
SignTool={#SignTool}
SignedUninstaller=yes
#endif

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked
Name: "autostart"; Description: "&Start RTSP Cam with Windows (in the tray)"; Flags: unchecked

[Files]
Source: "{#BinDir}\rtspcam.exe"; DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete
Source: "{#BinDir}\rtspcam_vcam.dll"; DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete regserver 64bit
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\RTSP Cam"; Filename: "{app}\rtspcam.exe"
Name: "{autodesktop}\RTSP Cam"; Filename: "{app}\rtspcam.exe"; Tasks: desktopicon

[Registry]
; Same value and command line the app's own "Start with Windows" setting writes.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "RtspCam"; ValueData: """{app}\rtspcam.exe"" --minimized"; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\rtspcam.exe"; Description: "Start RTSP Cam"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
; Safety net if the close request was ignored. The cameras vanish with the process.
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM rtspcam.exe"; Flags: runhidden; RunOnceId: "KillApp"

[Code]
// Frame Server keeps the DLL loaded while a camera is in use. Stopping it lets the file be
// replaced or deleted without a reboot; Windows starts it again when a camera is next opened.
procedure StopFrameServer;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\net.exe'), 'stop FrameServerMonitor /y', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\net.exe'), 'stop FrameServer /y', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopFrameServer;
  Result := '';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  // Runs before the files (and the DLL's unregistration) are processed.
  if CurUninstallStep = usUninstall then
    StopFrameServer;
end;
