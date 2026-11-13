; LiteLaunch x64 installer (Inno Setup 6).
; Build: installer\build.ps1  (or: cargo build --release, then ISCC installer\litelaunch.iss)
; Per-user install into %LOCALAPPDATA%\Programs\LiteLaunch, no admin rights needed.

#define AppName "LiteLaunch"
#define AppExe "litelaunch.exe"
#define SrcExe "..\target\release\litelaunch.exe"
; Plugin DLLs from scripts\build-plugins.ps1.
#define Plugins "..\target\plugins\out"
#ifndef AppVersion
  #define AppVersion GetVersionNumbersString(SrcExe)
#endif

[Setup]
AppId={{6F3C2A1E-9B7D-4E58-A2C4-1D8E5F0B7A93}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Slava Gusak
VersionInfoVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\target\installer
OutputBaseFilename=LiteLaunch-{#AppVersion}-x64-setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=no
MinVersion=10.0

[Languages]
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
ru.Autostart=Запускать при входе в Windows
en.Autostart=Start with Windows
ru.DeleteData=Удалить также настройки и данные плагинов (%APPDATA%\LiteLaunch)?
en.DeleteData=Also delete settings and plugin data (%APPDATA%\LiteLaunch)?
ru.TypeDefault=Обычная установка
en.TypeDefault=Default installation
ru.TypeCustom=Выборочная установка
en.TypeCustom=Custom installation
ru.Main=LiteLaunch: программы, калькулятор, процессы, Steam, буфер обмена
en.Main=LiteLaunch: apps, calculator, processes, Steam, clipboard
ru.Plugins=Плагины
en.Plugins=Plugins
ru.PluginVd=vd — скачивание видео (yt-dlp, нужен Python)
en.PluginVd=vd — video downloader (yt-dlp, needs Python)
ru.PluginYad=yad — скачивание с Яндекс Музыки (нужны Python и токен)
en.PluginYad=yad — Yandex Music downloader (needs Python and a token)
ru.PluginZapret=z — запуск zapret-discord-youtube (права администратора)
en.PluginZapret=z — zapret-discord-youtube launcher (needs admin rights)

[Types]
Name: "default"; Description: "{cm:TypeDefault}"
Name: "custom"; Description: "{cm:TypeCustom}"; Flags: iscustom

[Components]
Name: "main"; Description: "{cm:Main}"; Types: default custom; Flags: fixed
Name: "plugins"; Description: "{cm:Plugins}"; Types: default custom
Name: "plugins\vd"; Description: "{cm:PluginVd}"; Types: default custom
Name: "plugins\yad"; Description: "{cm:PluginYad}"
Name: "plugins\zapret"; Description: "{cm:PluginZapret}"

[Tasks]
Name: "autostart"; Description: "{cm:Autostart}"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SrcExe}"; DestDir: "{app}"; Flags: ignoreversion; Components: main
; Plugins live where the launcher looks for them; their data (tokens, libraries) goes next to them,
; in %APPDATA%\LiteLaunch\<id>, and is kept unless the user asks to delete it.
Source: "{#Plugins}\vd\vd.dll"; DestDir: "{userappdata}\{#AppName}\plugins\vd"; Flags: ignoreversion; Components: plugins\vd
Source: "{#Plugins}\yad\yad.dll"; DestDir: "{userappdata}\{#AppName}\plugins\yad"; Flags: ignoreversion; Components: plugins\yad
Source: "{#Plugins}\zapret\zapret.dll"; DestDir: "{userappdata}\{#AppName}\plugins\zapret"; Flags: ignoreversion; Components: plugins\zapret

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Same value the tray menu's "Автозапуск" toggles.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#AppName}"; ValueData: """{app}\{#AppExe}"""; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; The launcher's own updates run setup with /SILENT /restart=1: start it again afterwards.
Filename: "{app}\{#AppExe}"; Flags: nowait; Check: RestartAfterUpdate

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
function RestartAfterUpdate: Boolean;
begin
  Result := WizardSilent and (ExpandConstant('{param:restart|0}') = '1');
end;

// Close a running instance (litelaunch --exit) so its exe can be replaced or removed.
procedure ExitRunning(const Exe: String);
var
  Code, I: Integer;
begin
  if not FileExists(Exe) then Exit;
  Exec(Exe, '--exit', '', SW_HIDE, ewWaitUntilTerminated, Code);
  // The running instance needs a moment to quit and release the file.
  for I := 1 to 20 do begin
    if RenameFile(Exe, Exe + '.old') then begin
      RenameFile(Exe + '.old', Exe);
      Break;
    end;
    Sleep(150);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  ExitRunning(ExpandConstant('{app}\{#AppExe}'));
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  // Unticked autostart on reinstall: drop a value left by an earlier install or the tray menu.
  if (CurStep = ssPostInstall) and not WizardIsTaskSelected('autostart') then
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#AppName}');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then begin
    ExitRunning(ExpandConstant('{app}\{#AppExe}'));
    // The tray menu may have set autostart without the installer knowing.
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#AppName}');
  end;
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent then
    if MsgBox(CustomMessage('DeleteData'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      DelTree(ExpandConstant('{userappdata}\{#AppName}'), True, True, True);
end;
