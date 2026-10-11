; Tauri 安装器自定义文案（简体中文）。
;
; 文件名与语言标识由 `tauri.conf.json` 的 `nsis.customLanguageFiles` 指定：
; key 是 **NSIS 语言标识**（`SimpChinese` / `Japanese`，对应 NSIS 自带语言文件的名字），
; 值是这个文件。Tauri 自己的英文模板在同一个目录下按 `English` 找，所以英文不用给文件。
;
; ⚠️ 每条 LangString 的第一个参数必须是 `${LANG_SIMPCHINESE}` —— 写别的语言标识，
; 这条文案就挂到了另一个语言上（安装器显示中文、自定义文案却是英文）。
LangString addOrReinstall ${LANG_SIMPCHINESE} "添加 / 重新安装组件"
LangString alreadyInstalled ${LANG_SIMPCHINESE} "已经安装过了"
LangString alreadyInstalledLong ${LANG_SIMPCHINESE} "${PRODUCTNAME} ${VERSION} 已经装在这台机器上。选择要做什么，再点「下一步」。"
LangString appRunning ${LANG_SIMPCHINESE} "{{product_name}} 正在运行！请先关掉它再试。"
LangString appRunningOkKill ${LANG_SIMPCHINESE} "{{product_name}} 正在运行！$\n点「确定」结束它"
LangString chooseMaintenanceOption ${LANG_SIMPCHINESE} "选择要进行的维护操作。"
LangString choowHowToInstall ${LANG_SIMPCHINESE} "选择怎么安装 ${PRODUCTNAME}。"
LangString createDesktop ${LANG_SIMPCHINESE} "创建桌面快捷方式"
LangString dontUninstall ${LANG_SIMPCHINESE} "不卸载"
LangString dontUninstallDowngrade ${LANG_SIMPCHINESE} "不卸载（这个安装程序不允许「不卸载就降级」）"
LangString failedToKillApp ${LANG_SIMPCHINESE} "结束 {{product_name}} 失败。请先手动关掉它再试"
LangString installingWebview2 ${LANG_SIMPCHINESE} "正在安装 WebView2…"
LangString newerVersionInstalled ${LANG_SIMPCHINESE} "${PRODUCTNAME} 已经装了更新的版本！不建议再装旧版；真要装，最好先把当前这版卸掉。选择要做什么，再点「下一步」。"
LangString older ${LANG_SIMPCHINESE} "更旧"
LangString olderOrUnknownVersionInstalled ${LANG_SIMPCHINESE} "系统里装着一个 $R4 的 ${PRODUCTNAME}。建议先卸载当前版本再装。选择要做什么，再点「下一步」。"
LangString silentDowngrades ${LANG_SIMPCHINESE} "这个安装程序不允许降级，静默安装走不下去，请用图形界面安装。$\n"
LangString unableToUninstall ${LANG_SIMPCHINESE} "卸载失败！"
LangString uninstallApp ${LANG_SIMPCHINESE} "卸载 ${PRODUCTNAME}"
LangString uninstallBeforeInstalling ${LANG_SIMPCHINESE} "先卸载再安装"
LangString unknown ${LANG_SIMPCHINESE} "未知"
LangString webview2AbortError ${LANG_SIMPCHINESE} "WebView2 装不上！没有它程序跑不起来。重启一次安装程序再试。"
LangString webview2DownloadError ${LANG_SIMPCHINESE} "错误：下载 WebView2 失败 —— $0"
LangString webview2DownloadSuccess ${LANG_SIMPCHINESE} "WebView2 引导程序已下载"
LangString webview2Downloading ${LANG_SIMPCHINESE} "正在下载 WebView2 引导程序…"
LangString webview2InstallError ${LANG_SIMPCHINESE} "错误：安装 WebView2 失败，退出码 $1"
LangString webview2InstallSuccess ${LANG_SIMPCHINESE} "WebView2 已装好"
LangString deleteAppData ${LANG_SIMPCHINESE} "删除应用数据"
