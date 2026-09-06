#define MyAppName "SightOCR"
#ifndef MyAppVersion
#define MyAppVersion "2.0.0"
#endif
#define MyAppPublisher "FueTsui"
#define MyAppURL "https://github.com/FueTsui/SightOCR"
#define MyAppExeName "SightOCR.exe"
#ifndef PackageDir
#define PackageDir "dist\SightOCR"
#endif

[Setup]
#ifdef SmokeTest
AppId=SightOCR.InstallerSmoke
CreateUninstallRegKey=no
UsePreviousAppDir=no
UsePreviousTasks=no
DefaultDirName={#SmokeInstallDir}
#else
AppId={{B5261760-0D41-4798-9917-D5AA8C2510C8}}
DefaultDirName={localappdata}\Programs\{#MyAppName}
#endif
AppName={#MyAppName}
AppVersion={#MyAppVersion}
VersionInfoVersion={#MyAppVersion}
VersionInfoProductName={#MyAppName}
VersionInfoDescription={#MyAppName}
VersionInfoCompany={#MyAppPublisher}
VersionInfoCopyright=Copyright 2026 {#MyAppPublisher}. All rights reserved.
OutputBaseFilename={#MyAppName}-Setup-{#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
AppCopyright=Copyright 2026 {#MyAppPublisher}. All rights reserved.
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
SetupIconFile=assets\icon.ico
Compression=lzma2
SolidCompression=yes
OutputDir=dist\installer
UninstallDisplayIcon={app}\{#MyAppExeName}
PrivilegesRequired=lowest
MinVersion=10.0
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
DisableProgramGroupPage=yes
; Shutdown is scoped to the selected installation's executable in [Code].
CloseApplications=no
RestartApplications=no
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimplified"; MessagesFile: "scripts\installer\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "startmenu"; Description: "{cm:CreateStartMenuIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[CustomMessages]
english.CreateStartMenuIcon=Create a Start Menu shortcut
chinesesimplified.CreateStartMenuIcon=创建开始菜单快捷方式
english.CloseRunningApp=SightOCR is currently running. Click OK to close it automatically and continue. Unsaved recognition results will be lost. Click Cancel to exit Setup.
chinesesimplified.CloseRunningApp=安装程序发现 SightOCR 当前正在运行。点击“确定”将自动关闭程序并继续安装，未保存的识别内容将丢失；点击“取消”退出安装。
english.CloseRunningAppFailed=SightOCR could not exit within the allowed time. No files have been replaced. Exit SightOCR from its tray menu and retry.
chinesesimplified.CloseRunningAppFailed=SightOCR 未能在规定时间内退出，尚未替换任何文件。请通过托盘菜单退出 SightOCR 后重试。
english.ProcessCheckFailed=Unable to check running processes. No files have been replaced. Please retry Setup.
chinesesimplified.ProcessCheckFailed=无法检查正在运行的程序，尚未替换任何文件。请重新运行安装程序。

[Files]
Source: "{#PackageDir}\SightOCR.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\docs\*.md"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "{#PackageDir}\resources\oneocr\oneocr.dll"; DestDir: "{app}\resources\oneocr"; Flags: ignoreversion
Source: "{#PackageDir}\resources\oneocr\onnxruntime.dll"; DestDir: "{app}\resources\oneocr"; Flags: ignoreversion
Source: "{#PackageDir}\resources\oneocr\oneocr.onemodel"; DestDir: "{app}\resources\oneocr"; Flags: ignoreversion

#ifndef SmokeTest
[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\{#MyAppExeName}"; Tasks: startmenu
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\{#MyAppExeName}"; Tasks: desktopicon
#endif

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent; Check: not IsUpdateMode
Filename: "{app}\{#MyAppExeName}"; Flags: nowait; Check: IsUpdateMode

[Code]
const
  ProcessQueryLimitedInformation = $1000;
  SynchronizeAccess = $100000;
  ProcessTerminateAccess = $0001;
  WaitObject0 = 0;
  WaitTimeout = $102;

type
  TProcessIds = array[0..4095] of DWORD;

function EnumProcesses(var ProcessIds: TProcessIds; Size: DWORD;
  var BytesReturned: DWORD): BOOL;
  external 'EnumProcesses@psapi.dll stdcall';
function OpenProcess(Access: DWORD; InheritHandle: BOOL; ProcessId: DWORD): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function QueryFullProcessImageName(Process: THandle; Flags: DWORD;
  ImageName: String; var Size: DWORD): BOOL;
  external 'QueryFullProcessImageNameW@kernel32.dll stdcall';
function WaitForSingleObject(Handle: THandle; Milliseconds: DWORD): DWORD;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): BOOL;
  external 'CloseHandle@kernel32.dll stdcall';
function TerminateProcess(Process: THandle; ExitCode: UINT): BOOL;
  external 'TerminateProcess@kernel32.dll stdcall';
function FindWindowEx(Parent, After: HWND; ClassName, WindowName: Longint): HWND;
  external 'FindWindowExW@user32.dll stdcall';
function GetWindowThreadProcessId(Window: HWND; var ProcessId: DWORD): DWORD;
  external 'GetWindowThreadProcessId@user32.dll stdcall';
function GetProp(Window: HWND; Name: String): THandle;
  external 'GetPropW@user32.dll stdcall';
function RegisterWindowMessage(Name: String): UINT;
  external 'RegisterWindowMessageW@user32.dll stdcall';

var
  ShutdownCancelled: Boolean;

function IsUpdateMode: Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), '/UPDATE') = 0 then
      Result := True;
end;

function IsTargetProcess(Process: THandle): Boolean;
var
  Name: String;
  Size: DWORD;
begin
  Size := 32768;
  SetLength(Name, Size);
  Result := False;
  if not QueryFullProcessImageName(Process, 0, Name, Size) then
    Exit;
  SetLength(Name, Size);
  Result := CompareText(Name, ExpandConstant('{app}\{#MyAppExeName}')) = 0;
end;

procedure RequestGracefulExit(ProcessId: DWORD);
var
  Window: HWND;
  WindowProcess: DWORD;
  MessageId: UINT;
begin
  MessageId := RegisterWindowMessage('SightOCR.MainWindow.Exit');
  if MessageId = 0 then
    Exit;
  Window := FindWindowEx(0, 0, 0, 0);
  while Window <> 0 do begin
    GetWindowThreadProcessId(Window, WindowProcess);
    if (WindowProcess = ProcessId) and
       (GetProp(Window, 'SightOCR.MainWindow.ExitSupported') <> 0) then
      PostMessage(Window, MessageId, 0, 0);
    Window := FindWindowEx(0, Window, 0, 0);
  end;
end;

function CloseRunningApp: String;
var
  ProcessIds: TProcessIds;
  BytesReturned, I, WaitResult, Deadline: DWORD;
  Process, TerminatingProcess: THandle;
  Approved: Boolean;
begin
  Result := '';
  Approved := IsUpdateMode;
  if not EnumProcesses(ProcessIds, 16384, BytesReturned) then begin
    Result := CustomMessage('ProcessCheckFailed');
    Exit;
  end;
  if BytesReturned >= 16384 then begin
    Result := CustomMessage('ProcessCheckFailed');
    Exit;
  end;
  if BytesReturned = 0 then
    Exit;
  for I := 0 to (BytesReturned div 4) - 1 do begin
    Process := OpenProcess(ProcessQueryLimitedInformation or SynchronizeAccess,
      False, ProcessIds[I]);
    if Process = 0 then
      Continue;
    try
      if not IsTargetProcess(Process) or
         (WaitForSingleObject(Process, 0) = WaitObject0) then
        Continue;
      if not Approved then begin
        Approved := SuppressibleMsgBox(CustomMessage('CloseRunningApp'),
          mbConfirmation, MB_OKCANCEL, IDCANCEL) = IDOK;
        if not Approved then begin
          ShutdownCancelled := True;
          Result := CustomMessage('CloseRunningAppFailed');
          Exit;
        end;
      end;
      Log(Format('Requesting shutdown of installation process %d.', [ProcessIds[I]]));
      RequestGracefulExit(ProcessIds[I]);
      if IsUpdateMode then
        Deadline := 120000
      else
        Deadline := 5000;
      WaitResult := WaitForSingleObject(Process, Deadline);
      if (WaitResult = WaitTimeout) and not IsUpdateMode then begin
        { Older releases do not expose the graceful-exit message. The user has
          explicitly approved closing this installation. Recheck the new handle
          and retain the original handle so PID reuse cannot target another app. }
        TerminatingProcess := OpenProcess(ProcessQueryLimitedInformation or
          ProcessTerminateAccess, False, ProcessIds[I]);
        if TerminatingProcess <> 0 then begin
          try
            if IsTargetProcess(TerminatingProcess) and
               (WaitForSingleObject(Process, 0) = WaitTimeout) then begin
              Log('Closing legacy/unresponsive SightOCR after confirmation.');
              TerminateProcess(TerminatingProcess, 0);
            end;
          finally
            CloseHandle(TerminatingProcess);
          end;
        end;
        WaitResult := WaitForSingleObject(Process, 10000);
      end;
      if WaitResult <> WaitObject0 then begin
        Log('SightOCR did not exit; installation stopped before file replacement.');
        Result := CustomMessage('CloseRunningAppFailed');
        Exit;
      end;
      Log('SightOCR process exited.');
    finally
      CloseHandle(Process);
    end;
  end;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Error: String;
begin
  Result := True;
  if CurPageID <> wpReady then
    Exit;
  Error := CloseRunningApp;
  Result := Error = '';
  if ShutdownCancelled then
    WizardForm.Close
  else if not Result then
    SuppressibleMsgBox(Error, mbError, MB_OK, IDOK);
end;

procedure CancelButtonClick(CurPageID: Integer; var Cancel, Confirm: Boolean);
begin
  if ShutdownCancelled then begin
    Cancel := True;
    Confirm := False;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  { Also catches silent installs and an app started after the Ready page. }
  Result := CloseRunningApp;
end;

function InitializeUninstall: Boolean;
var
  Error: String;
begin
  Error := CloseRunningApp;
  Result := Error = '';
  if not Result and not ShutdownCancelled then
    SuppressibleMsgBox(Error, mbError, MB_OK, IDOK);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Command: String;
  Expected: String;
begin
  if CurUninstallStep <> usUninstall then
    Exit;
  { Preserve configuration and other installations' startup entries. }
  Expected := '"' + ExpandConstant('{app}\{#MyAppExeName}') + '" --silent';
  if RegQueryStringValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run',
    '{#MyAppName}', Command) and (CompareText(Command, Expected) = 0) then
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#MyAppName}');
end;

