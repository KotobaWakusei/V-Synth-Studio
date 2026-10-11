; 安装前先清掉同一产品的 **MSI（WiX）** 版本。
;
; Tauri 生成的 installer.nsi 自己带一套 WiX 检测（`PageReinstall`），但它走的是
; 「用 MSI 的 UninstallString 起 msiexec」那条路，实测（本机：HKLM 里同时留着
; MSI 1.4.0 与 NSIS 1.4.0 两条清单）没能把它清掉 —— 于是 Program Files 里那份
; 旧版一直躺着，用户在「应用和功能」里看到两个 v-synth-studio。
;
; 这里改用 Windows Installer 的 API 找：枚举 `HKLM\...\Uninstall`，凡是
; `DisplayName` 与 `Publisher` 都和本产品一致、且 `WindowsInstaller=1`
; 的那条，就用它的 `{ProductCode}` 静默卸载。**只找 HKLM** —— WiX 那套是
; per-machine 装的，HKCU 里的是 NSIS 自己（那个由 installer.nsi 的重装页处理）。
;
; 卸载失败（用户取消 UAC、别的安装正在进行）不拦安装：新版照装，留一条日志。
; 两个都留着用同一个数据目录，不会丢配置，只是占磁盘。
!macro NSIS_HOOK_PREINSTALL
  Push $R0
  Push $R1
  Push $R2
  Push $R3

  ; 安装时选定的界面语言交给程序：程序第一次启动按它设自己的界面语言
  ; （用户之后在「设置」里改过的，程序这边以配置为准，不再覆盖）。
  ; 三条分开写而不是循环：WriteRegStr 的键名要常量，而语言标识只有三种。
  ${If} $LANGUAGE == ${LANG_SIMPCHINESE}
    WriteRegStr SHCTX "${MANUPRODUCTKEY}" "InstallerLanguage" "zh-CN"
  ${ElseIf} $LANGUAGE == ${LANG_JAPANESE}
    WriteRegStr SHCTX "${MANUPRODUCTKEY}" "InstallerLanguage" "ja-JP"
  ${Else}
    WriteRegStr SHCTX "${MANUPRODUCTKEY}" "InstallerLanguage" "en-US"
  ${EndIf}

  StrCpy $R2 0

  msi_loop:
    EnumRegKey $R1 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall" $R2
    StrCmp $R1 "" msi_done
    IntOp $R2 $R2 + 1

    ReadRegStr $R0 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$R1" "DisplayName"
    ; StrCmp 不分大小写，所以这里不必自己大小写归一
    StrCmp $R0 "${PRODUCTNAME}" 0 msi_loop
    ReadRegStr $R0 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$R1" "Publisher"
    StrCmp $R0 "${MANUFACTURER}" 0 msi_loop
    ReadRegStr $R0 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$R1" "WindowsInstaller"
    StrCmp $R0 "1" 0 msi_loop

    DetailPrint "正在卸载旧的 MSI 版本（$R1）…"
    ExecWait '"msiexec.exe" /x $R1 /qn /norestart' $R3
    DetailPrint "旧 MSI 卸载结束，退出码 $R3"
    Goto msi_loop

  msi_done:
  Pop $R3
  Pop $R2
  Pop $R1
  Pop $R0
!macroend
