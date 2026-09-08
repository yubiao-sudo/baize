; 白泽 NSIS 安装器钩子：
; 升级/卸载流程会清空 $INSTDIR（含运行时数据目录 data —— 数据库/浏览器登录态/截图缓存），
; 这里在卸载前把 data 备份到 LocalAppData，安装完成后回填，
; 保证模型、语音、记忆、用量等用户数据在升级或重装后原样保留。
; （与 Rust 侧 paths.rs 的 backup_to_appdata / migrate_legacy 互为双保险）

!include "LogicLib.nsh"

; 卸载前（升级安装器会静默先卸载旧版）：备份 data
!macro NSIS_HOOK_PREUNINSTALL
  ${If} ${FileExists} "$INSTDIR\data\baize.db"
    RMDir /r "$LOCALAPPDATA\baize\update-backup"
    CreateDirectory "$LOCALAPPDATA\baize\update-backup"
    CopyFiles /SILENT "$INSTDIR\data\*.*" "$LOCALAPPDATA\baize\update-backup"
  ${EndIf}
!macroend

; 安装完成后：若 data 缺主库（升级/重装场景）且有备份，则回填
!macro NSIS_HOOK_POSTINSTALL
  ${Unless} ${FileExists} "$INSTDIR\data\baize.db"
    ${If} ${FileExists} "$LOCALAPPDATA\baize\update-backup\baize.db"
      CreateDirectory "$INSTDIR\data"
      CopyFiles /SILENT "$LOCALAPPDATA\baize\update-backup\*.*" "$INSTDIR\data"
    ${EndIf}
  ${EndUnless}
!macroend
