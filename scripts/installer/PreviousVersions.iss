{ Only registered SightOCR installations are removed. Never recursively delete
  an installation directory: user files and configuration must survive. }
type
  TOldInstallation = record
    Directory: String;
    Uninstaller: String;
  end;
  TOldRegistration = record
    Root: Integer;
    Key: String;
    Directory: String;
    Command: String;
  end;
var
  OldInstallations: array of TOldInstallation;
  OldRegistrations: array of TOldRegistration;
  OldVersionsRemoved: Boolean;
  RestoreAdminAfterUpdate: Boolean;
  RestoreStartupAfterUpdate: Boolean;

function AddPreviousVersion(Root: Integer; Key: String): String;
var
  DisplayName, Directory, Command, Uninstaller, FileName: String;
  I, Count, Group: Integer;
  ValidUninstaller: Boolean;
begin
  Result := '';
  if not RegQueryStringValue(Root, Key, 'DisplayName', DisplayName) then Exit;
  if (CompareText(DisplayName, 'SightOCR') <> 0) and
     (CompareText(Copy(DisplayName, 1, 9), 'SightOCR ') <> 0) then Exit;
  if not RegQueryStringValue(Root, Key, 'InstallLocation', Directory) or
     (Trim(Directory) = '') then begin
    Result := '旧版 SightOCR 安装登记不完整，请先从 Windows 设置卸载旧版。';
    Exit;
  end;
  Directory := RemoveBackslashUnlessRoot(ExpandFileName(Directory));
  if Length(Directory) <= 3 then begin
    Result := '旧版 SightOCR 安装目录无效：' + Directory;
    Exit;
  end;
  if not RegQueryStringValue(Root, Key, 'UninstallString', Command) then Command := '';
  Uninstaller := RemoveQuotes(Trim(Command));
  FileName := ExtractFileName(Uninstaller);
  ValidUninstaller := (CompareText(ExtractFileDir(Uninstaller), Directory) = 0) and
    (CompareText(Copy(FileName, 1, 5), 'unins') = 0) and
    (CompareText(ExtractFileExt(FileName), '.exe') = 0) and FileExists(Uninstaller);
  { Collect every registration before deciding whether a directory is broken.
    A missing HKLM unins001 must not veto a valid HKCU unins000 (or vice versa). }
  Count := GetArrayLength(OldRegistrations);
  SetArrayLength(OldRegistrations, Count + 1);
  OldRegistrations[Count].Root := Root;
  OldRegistrations[Count].Key := Key;
  OldRegistrations[Count].Directory := Directory;
  OldRegistrations[Count].Command := Command;
  Group := -1;
  for I := 0 to GetArrayLength(OldInstallations) - 1 do
    if CompareText(OldInstallations[I].Directory, Directory) = 0 then Group := I;
  if Group < 0 then begin
    Group := GetArrayLength(OldInstallations);
    SetArrayLength(OldInstallations, Group + 1);
    OldInstallations[Group].Directory := Directory;
  end;
  if ValidUninstaller and (OldInstallations[Group].Uninstaller = '') then
    OldInstallations[Group].Uninstaller := Uninstaller;
  Log('Found SightOCR registration: ' + Key + ' -> ' + Directory);
end;

function HasInstalledProgram(Directory: String): Boolean;
begin
  Result := FileExists(Directory + '\SightOCR.exe') or
    FileExists(Directory + '\sightocr-mcp.exe') or
    FileExists(Directory + '\sightocr-cli.exe');
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
  I: Integer;
begin
  Result := '';
  if OldVersionsRemoved then Exit;
  SetArrayLength(OldInstallations, 0);
  SetArrayLength(OldRegistrations, 0);
#ifdef SmokeTest
  Base := 'Software\SightOCR.InstallerSmoke\Uninstall';
  Result := FindPreviousVersionsInRoot(HKCU, Base);
  if Result = '' then
    Result := FindPreviousVersionsInRoot(HKCU, 'Software\SightOCR.InstallerSmoke\MachineUninstall');
#else
  Base := 'Software\Microsoft\Windows\CurrentVersion\Uninstall';
  Result := FindPreviousVersionsInRoot(HKCU64, Base);
  if Result = '' then Result := FindPreviousVersionsInRoot(HKCU32, Base);
  if Result = '' then Result := FindPreviousVersionsInRoot(HKLM64, Base);
  if Result = '' then Result := FindPreviousVersionsInRoot(HKLM32, Base);
#endif
  if Result <> '' then Exit;
  for I := 0 to GetArrayLength(OldInstallations) - 1 do
    if (OldInstallations[I].Uninstaller = '') and
       HasInstalledProgram(OldInstallations[I].Directory) then begin
      Result := '旧版 SightOCR 程序仍在，但没有可用的卸载程序：' +
        OldInstallations[I].Directory;
      Exit;
    end;
end;

procedure ApplyLegacyUpdateDestination;
var
  Requested, UserDefault, MachineDefault, NewDefault: String;
begin
  if not IsUpdateMode then Exit;
  Requested := RemoveBackslashUnlessRoot(ExpandFileName(WizardDirValue));
#ifdef SmokeTest
  UserDefault := ExpandConstant('{#SmokeInstallDir}\legacy-user');
  MachineDefault := ExpandConstant('{#SmokeInstallDir}\legacy-x86');
  NewDefault := ExpandConstant('{#SmokeInstallDir}\program-files');
#else
  UserDefault := ExpandConstant('{localappdata}\Programs\SightOCR');
  MachineDefault := ExpandConstant('{commonpf32}\SightOCR');
  NewDefault := ExpandConstant('{commonpf64}\SightOCR');
#endif
  if (CompareText(Requested, UserDefault) = 0) or
     (CompareText(Requested, MachineDefault) = 0) then begin
    Log('Migrating legacy update destination: ' + Requested + ' -> ' + NewDefault);
    WizardForm.DirEdit.Text := NewDefault;
  end;
end;

function RemoveObsoleteRegistrations: String;
var
  I: Integer;
  Directory, Command: String;
begin
  Result := '';
  for I := 0 to GetArrayLength(OldRegistrations) - 1 do begin
    if not RegKeyExists(OldRegistrations[I].Root, OldRegistrations[I].Key) then Continue;
    if not RegQueryStringValue(OldRegistrations[I].Root, OldRegistrations[I].Key,
      'InstallLocation', Directory) then begin
      Result := '旧版安装登记在卸载期间发生变化，已停止清理。';
      Exit;
    end;
    if not RegQueryStringValue(OldRegistrations[I].Root, OldRegistrations[I].Key,
      'UninstallString', Command) then Command := '';
    Directory := RemoveBackslashUnlessRoot(ExpandFileName(Directory));
    if (CompareText(Directory, OldRegistrations[I].Directory) <> 0) or
       (CompareText(Command, OldRegistrations[I].Command) <> 0) or
       HasInstalledProgram(Directory) then begin
      Result := '旧版安装登记或程序文件在卸载期间发生变化，已停止清理。';
      Exit;
    end;
    if not RegDeleteKeyIncludingSubkeys(OldRegistrations[I].Root, OldRegistrations[I].Key) then begin
      Result := '无法清除已卸载旧版的安装登记：' + OldRegistrations[I].Key;
      Exit;
    end;
  end;
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
    if OldInstallations[I].Uninstaller = '' then Continue;
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
    if OldInstallations[I].Uninstaller = '' then Continue;
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
    if HasInstalledProgram(OldInstallations[I].Directory) then begin
      Result := '旧版程序文件仍然存在，安装已停止，请先完成旧版卸载。';
      Exit;
    end;
  end;
  Result := RemoveObsoleteRegistrations;
  if Result <> '' then Exit;
  OldVersionsRemoved := True;
end;
