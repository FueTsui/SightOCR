{ Only registered SightOCR installations are removed. Never recursively delete
  an installation directory: user files and configuration must survive. }
type
  TOldInstallation = record
    Directory: String;
    Uninstaller: String;
  end;
var
  OldInstallations: array of TOldInstallation;
  OldVersionsRemoved: Boolean;
  RestoreAdminAfterUpdate: Boolean;
  RestoreStartupAfterUpdate: Boolean;

function AddPreviousVersion(Root: Integer; Key: String): String;
var
  DisplayName, Directory, Command, Uninstaller, FileName: String;
  I, Count: Integer;
begin
  Result := '';
  if not RegQueryStringValue(Root, Key, 'DisplayName', DisplayName) then Exit;
  if (CompareText(DisplayName, 'SightOCR') <> 0) and
     (CompareText(Copy(DisplayName, 1, 9), 'SightOCR ') <> 0) then Exit;
  if not RegQueryStringValue(Root, Key, 'InstallLocation', Directory) or
     not RegQueryStringValue(Root, Key, 'UninstallString', Command) then begin
    Result := '旧版 SightOCR 安装登记不完整，请先从 Windows 设置卸载旧版。';
    Exit;
  end;
  Directory := RemoveBackslashUnlessRoot(ExpandFileName(Directory));
  Uninstaller := RemoveQuotes(Trim(Command));
  FileName := ExtractFileName(Uninstaller);
  if (Length(Directory) <= 3) or
     (CompareText(ExtractFileDir(Uninstaller), Directory) <> 0) or
     (CompareText(Copy(FileName, 1, 5), 'unins') <> 0) or
     (CompareText(ExtractFileExt(FileName), '.exe') <> 0) or
     not FileExists(Uninstaller) then begin
    Result := '旧版 SightOCR 卸载程序无效或缺失，请先修复旧版安装。';
    Exit;
  end;
  for I := 0 to GetArrayLength(OldInstallations) - 1 do
    if CompareText(OldInstallations[I].Directory, Directory) = 0 then Exit;
  Count := GetArrayLength(OldInstallations);
  SetArrayLength(OldInstallations, Count + 1);
  OldInstallations[Count].Directory := Directory;
  OldInstallations[Count].Uninstaller := Uninstaller;
end;

function FindPreviousVersionsInRoot(Root: Integer; Base: String): String;
begin
  { Historical AppId escaped the opening brace but retained both closing
    braces. Match the actual installed key and the canonical GUID spelling. }
  Result := AddPreviousVersion(Root, Base + '\{B5261760-0D41-4798-9917-D5AA8C2510C8}}_is1');
  if Result = '' then
    Result := AddPreviousVersion(Root, Base + '\{B5261760-0D41-4798-9917-D5AA8C2510C8}_is1');
end;

function FindPreviousVersions: String;
var
  Base: String;
begin
  Result := '';
  if OldVersionsRemoved then Exit;
  SetArrayLength(OldInstallations, 0);
#ifdef SmokeTest
  Base := 'Software\SightOCR.InstallerSmoke\Uninstall';
  Result := FindPreviousVersionsInRoot(HKCU, Base);
#else
  Base := 'Software\Microsoft\Windows\CurrentVersion\Uninstall';
  Result := FindPreviousVersionsInRoot(HKCU64, Base);
  if Result = '' then Result := FindPreviousVersionsInRoot(HKCU32, Base);
  if Result = '' then Result := FindPreviousVersionsInRoot(HKLM64, Base);
  if Result = '' then Result := FindPreviousVersionsInRoot(HKLM32, Base);
#endif
end;

function IsPreviousExecutable(Name: String; ConsoleOnly: Boolean): Boolean;
var
  I: Integer;
  Directory, FileName: String;
begin
  Result := False;
  Directory := ExtractFileDir(Name);
  FileName := ExtractFileName(Name);
  if not (((not ConsoleOnly) and (CompareText(FileName, 'SightOCR.exe') = 0)) or
      (CompareText(FileName, 'sightocr-mcp.exe') = 0) or
      (CompareText(FileName, 'sightocr-cli.exe') = 0)) then Exit;
  for I := 0 to GetArrayLength(OldInstallations) - 1 do
    if CompareText(Directory, OldInstallations[I].Directory) = 0 then Result := True;
end;

function RemovePreviousVersions: String;
var
  I, ExitCode: Integer;
  Directory, Backup, Name, Startup: String;
  J: Integer;
begin
  Result := '';
  if OldVersionsRemoved then Exit;
  { Back up legacy configuration before any uninstall, retaining credentials
    in the user's local profile rather than in the public installation folder. }
  for I := 0 to GetArrayLength(OldInstallations) - 1 do begin
    Directory := OldInstallations[I].Directory;
    if IsUpdateMode and FileExists(Directory + '\run-as-admin') then
      RestoreAdminAfterUpdate := True;
    if IsUpdateMode and RegQueryStringValue(HKCU, '{#StartupKey}', 'SightOCR', Startup) then
      if CompareText(Startup, '"' + Directory + '\SightOCR.exe" --silent') = 0 then
        RestoreStartupAfterUpdate := True;
#ifdef SmokeTest
    Backup := ExpandConstant('{app}\upgrade-backup\') +
#else
    Backup := ExpandConstant('{localappdata}\SightOCR\UpgradeBackup\') +
#endif
      GetDateTimeString('yyyymmdd-hhnnss', '-', ':') + '-' + IntToStr(I);
    for J := 0 to 1 do begin
      if J = 0 then Name := 'config.json' else Name := 'paths.json';
      if FileExists(Directory + '\' + Name) then
        if not ForceDirectories(Backup) or
           not CopyFile(Directory + '\' + Name, Backup + '\' + Name, True) then begin
          Result := '无法备份旧版配置，尚未开始卸载。';
          Exit;
        end;
    end;
  end;
  for I := 0 to GetArrayLength(OldInstallations) - 1 do begin
    Log('Uninstalling registered SightOCR: ' + OldInstallations[I].Directory);
    if not Exec(OldInstallations[I].Uninstaller,
      '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART', '', SW_HIDE,
      ewWaitUntilTerminated, ExitCode) then begin
      Result := '无法启动旧版卸载程序，安装已停止。';
      Exit;
    end;
    if ExitCode <> 0 then begin
      Result := '旧版卸载未成功（退出码 ' + IntToStr(ExitCode) + '），安装已停止。';
      Exit;
    end;
    if FileExists(OldInstallations[I].Directory + '\SightOCR.exe') or
       FileExists(OldInstallations[I].Directory + '\sightocr-mcp.exe') then begin
      Result := '旧版程序文件仍然存在，安装已停止，请先完成旧版卸载。';
      Exit;
    end;
  end;
  OldVersionsRemoved := True;
end;
