; 拼音输入法安装程序（Inno Setup 6）。GitHub 自动编译时生成，也可以在 Windows 上手动：
;   iscc installer\pinyin.iss
; 需要先把文件放到 dist\stage（CI 里"整理安装文件"那一步做的事）：
;   pinyin-engine.exe  pinyin-settings.exe  pinyin-tsf64.dll  pinyin-tsf32.dll  pinyin.ico  pinyin.png
;   data\pinyin.dict  data\english.tsv  data\pinyin.lm
;
; 安装做的事：
;   1. 复制文件到 C:\Program Files\PinyinIME
;   2. 注册输入法（64 位和 32 位程序都能用）
;   3. 把"拼音"加到你的输入法列表（不改默认输入法）
;   4. 登录时自动启动引擎（后台进程，无窗口），并且现在就启动
;   5. 桌面和开始菜单放"拼音输入法设置"
; 不会带上任何别的电脑上的词语记忆；你的记忆和设置在 AppData\Roaming\PinyinIME，卸载时保留。

#ifndef AppVer
  #define AppVer "0.1.0"
#endif

[Setup]
AppId={{B3E8C9A0-5D2F-4C61-9E7A-2F4B6D8A1C35}
AppName=拼音输入法
AppVersion={#AppVer}
AppVerName=拼音输入法 {#AppVer}
AppPublisher=自用
DefaultDirName={autopf}\PinyinIME
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=..\dist
OutputBaseFilename=PinyinIME-Setup-{#AppVer}
Compression=lzma2/max
SolidCompression=yes
SetupIconFile=..\assets\icon\pinyin.ico
UninstallDisplayIcon={app}\pinyin.ico
UninstallDisplayName=拼音输入法
WizardStyle=modern
CloseApplications=no
ShowLanguageDialog=no

[Languages]
Name: "zh"; MessagesFile: "compiler:Default.isl,zh.isl"

[Files]
Source: "..\dist\stage\pinyin-engine.exe";   DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete
Source: "..\dist\stage\pinyin-settings.exe"; DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete
Source: "..\dist\stage\pinyin.ico";          DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\stage\pinyin.png";          DestDir: "{app}"; Flags: ignoreversion
; 输入法 DLL：会被正在运行的程序加载，所以文件被占用时重启后再替换
Source: "..\dist\stage\pinyin-tsf64.dll";    DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete regserver 64bit
Source: "..\dist\stage\pinyin-tsf32.dll";    DestDir: "{app}"; Flags: ignoreversion restartreplace uninsrestartdelete regserver 32bit
Source: "..\dist\stage\data\*";              DestDir: "{app}\data"; Flags: ignoreversion restartreplace uninsrestartdelete

[Registry]
; 每个用户登录时启动自己的引擎
Root: HKLM; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "PinyinIME"; ValueData: """{app}\pinyin-engine.exe"""; Flags: uninsdeletevalue

[Icons]
Name: "{autodesktop}\拼音输入法设置";  Filename: "{app}\pinyin-settings.exe"; IconFilename: "{app}\pinyin.ico"; Comment: "中文标点、添加词语"
Name: "{autoprograms}\拼音输入法设置"; Filename: "{app}\pinyin-settings.exe"; IconFilename: "{app}\pinyin.ico"; Comment: "中文标点、添加词语"

[Run]
; 以你本人的身份（不是管理员身份）运行：输入法列表是每个人自己的，引擎也必须是普通权限
Filename: "{app}\pinyin-settings.exe"; Parameters: "--enable"; Flags: runasoriginaluser runhidden waituntilterminated; StatusMsg: "正在把“拼音”加到你的输入法列表..."
Filename: "{app}\pinyin-engine.exe"; Flags: runasoriginaluser nowait; StatusMsg: "正在启动引擎..."
Filename: "{app}\pinyin-settings.exe"; Description: "打开设置"; Flags: postinstall nowait skipifsilent runasoriginaluser unchecked

[UninstallRun]
Filename: "{app}\pinyin-settings.exe"; Parameters: "--disable"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveTip"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM pinyin-engine.exe"; Flags: runhidden waituntilterminated; RunOnceId: "StopEngine"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM pinyin-settings.exe"; Flags: runhidden waituntilterminated; RunOnceId: "StopSettings"

[Code]
// 升级时先停掉正在运行的引擎和设置页，文件才能覆盖
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM pinyin-engine.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM pinyin-settings.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := '';
end;
