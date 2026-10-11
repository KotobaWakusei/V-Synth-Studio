; 安装前把安装时选的语言交给程序，并清掉同一产品的 **MSI（WiX）** 旧版。
;
; ── 语言交接 ────────────────────────────────────────────────────────
; NSIS 的语言选择框会把结果写进 `$LANGUAGE`，而这个钩子在安装开始前跑
; （生成的 installer.nsi 里：`MUI_LANGDLL_DISPLAY` 在第 490 行、本钩子在第 638 行），
; 所以这里读得到。程序启动时读这个值决定界面语言，用户在「设置」里改过之后以配置为准。
!macro NSIS_HOOK_PREINSTALL
  Push $R0
  Push $R1
  Push $R2
  Push $R3

  ${If} $LANGUAGE == ${LANG_SIMPCHINESE}
    WriteRegStr SHCTX "${MANUPRODUCTKEY}" "InstallerLanguage" "zh-CN"
  ${ElseIf} $LANGUAGE == ${LANG_JAPANESE}
    WriteRegStr SHCTX "${MANUPRODUCTKEY}" "InstallerLanguage" "ja-JP"
  ${Else}
    WriteRegStr SHCTX "${MANUPRODUCTKEY}" "InstallerLanguage" "en-US"
  ${EndIf}

  ; ── 清掉旧的 MSI 版本 ──────────────────────────────────────────────
  ; 1.4.0 以前只发 MSI，那份是 per-machine 装在 `C:\Program Files` 的；不清掉的话
  ; 「应用和功能」里会留两条 v-synth-studio、磁盘上多一份 75 MB 的旧程序
  ; （数据目录共用，所以不影响设置）。
  ;
  ; ⚠️ **为什么不用 Tauri 模板自带的 WiX 检测**：它走「用 MSI 的 UninstallString 起
  ; msiexec」，而安装器是 per-user、不提权，卸载 per-machine 的 MSI 会被系统拒 ——
  ; 表现是「卸载失败，要重开安装器」。这里改成按需提权跑一次。
  ;
  ; ⚠️ **为什么是 `-EncodedCommand` 而不是 `-Command "…"`**：NSIS 的解析器把裸 `"`
  ; 当字符串结束（连 `''` 里那几个也认），`ExecWait` 会把它切成 4 个参数并报
  ; `ExecWait expects 1-2 parameters, got 4`。编码成 base64 之后整条命令**没有引号**，
  ; 也不受 `$` / 反引号影响。下面这段 base64 解码回来是：
  ;
  ;   Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall',
  ;     'HKLM:\SOFTWARE\WOW6432Node\...\Uninstall' -ErrorAction SilentlyContinue |
  ;   Where-Object { $_.DisplayName -eq 'v-synth-studio' -and $_.Publisher -eq 'qingmu'
  ;                  -and $_.WindowsInstaller -eq 1 } |
  ;   ForEach-Object { Start-Process msiexec.exe -ArgumentList @('/x', $_.PSChildName,
  ;     '/qn', '/norestart') -Wait }
  ;
  ; ⚠️ 只找 HKLM（per-machine 的 MSI 在那儿）；HKCU 里那条是本程序自己，覆盖安装即可。
  ; ⚠️ 会弹一次 UAC —— 卸载 per-machine 的 MSI 必须要管理员。干净机器上枚举不到条目，
  ; 一声不响。用户点「否」就让新版照装（留一条日志），不拦安装。
  DetailPrint "正在清理旧的 MSI 版本…"
  ExecWait 'powershell.exe -NoProfile -ExecutionPolicy Bypass -EncodedCommand RwBlAHQALQBDAGgAaQBsAGQASQB0AGUAbQAgACcASABLAEwATQA6AFwAUwBPAEYAVABXAEEAUgBFAFwATQBpAGMAcgBvAHMAbwBmAHQAXABXAGkAbgBkAG8AdwBzAFwAQwB1AHIAcgBlAG4AdABWAGUAcgBzAGkAbwBuAFwAVQBuAGkAbgBzAHQAYQBsAGwAJwAsACcASABLAEwATQA6AFwAUwBPAEYAVABXAEEAUgBFAFwAVwBPAFcANgA0ADMAMgBOAG8AZABlAFwATQBpAGMAcgBvAHMAbwBmAHQAXABXAGkAbgBkAG8AdwBzAFwAQwB1AHIAcgBlAG4AdABWAGUAcgBzAGkAbwBuAFwAVQBuAGkAbgBzAHQAYQBsAGwAJwAgAC0ARQByAHIAbwByAEEAYwB0AGkAbwBuACAAUwBpAGwAZQBuAHQAbAB5AEMAbwBuAHQAaQBuAHUAZQAgAHwAIABXAGgAZQByAGUALQBPAGIAagBlAGMAdAAgAHsAIAAkAF8ALgBEAGkAcwBwAGwAYQB5AE4AYQBtAGUAIAAtAGUAcQAgACcAdgAtAHMAeQBuAHQAaAAtAHMAdAB1AGQAaQBvACcAIAAtAGEAbgBkACAAJABfAC4AUAB1AGIAbABpAHMAaABlAHIAIAAtAGUAcQAgACcAcQBpAG4AZwBtAHUAJwAgAC0AYQBuAGQAIAAkAF8ALgBXAGkAbgBkAG8AdwBzAEkAbgBzAHQAYQBsAGwAZQByACAALQBlAHEAIAAxACAAfQAgAHwAIABGAG8AcgBFAGEAYwBoAC0ATwBiAGoAZQBjAHQAIAB7ACAAUwB0AGEAcgB0AC0AUAByAG8AYwBlAHMAcwAgAG0AcwBpAGUAeABlAGMALgBlAHgAZQAgAC0AQQByAGcAdQBtAGUAbgB0AEwAaQBzAHQAIABAACgAJwAvAHgAJwAsACAAJABfAC4AUABTAEMAaABpAGwAZABOAGEAbQBlACwAIAAnAC8AcQBuACcALAAgACcALwBuAG8AcgBlAHMAdABhAHIAdAAnACkAIAAtAFcAYQBpAHQAIAB9AA==' $R3
  DetailPrint "旧 MSI 清理结束，退出码 $R3"

  Pop $R3
  Pop $R2
  Pop $R1
  Pop $R0
!macroend
