<#
    读 MSI 自己的 File 表，按 JSON 吐给 stdout。

        powershell -NoProfile -File tools/msi-file-table.ps1 -Msi <路径>
        → {"rows":1395,"total":274000000,"sizes":{"v-synth-studio.exe":62000000,...}}

    这条走 WindowsInstaller COM：不需要桌面、不装东西、秒级。**判据不写在这里** ——
    只负责读表，好不好由 tools/smoke-package.mjs 判，两边改一处忘一处的机会就少一处。

    ⚠️ 三条姿势：
      · SQL 的列名要**反引号**；
      · `InvokeMember` 的每个返回值都要 `$null =` 接住，否则会混进函数输出、JSON 解析不了；
      · 输出编码要显式设成 UTF-8，否则 5.1 下按控制台代码页走，中文文件名会变问号。
#>
param([Parameter(Mandatory = $true)][string]$Msi)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$installer = New-Object -ComObject WindowsInstaller.Installer
$db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Msi, 0))
$view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @('SELECT `File`, `FileName`, `FileSize` FROM `File`'))
$null = $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null)

# 一遍收完：行数、名字、逐个体积、总体积。别为了拿某一项的体积再查一遍。
# ⚠️ 「多少文件」数**行数**，不能数 $sizes.Count：FileName 不是唯一的（libresvip 那棵
#    Python 树里 __init__.py 出现 51 次），按名字数会少算 299 个。
$sizes = @{}
$rows = 0
$total = 0
while ($true) {
  $rec = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
  if (-not $rec) { break }
  $rows++
  $fn = [string]$rec.GetType().InvokeMember('StringData', 'GetProperty', $null, $rec, 2)
  $sz = [int]$rec.GetType().InvokeMember('IntegerData', 'GetProperty', $null, $rec, 3)
  $total += $sz
  if ($fn -match '\|') { $fn = ($fn -split '\|')[-1] }   # FileName 是「短名|长名」
  $sizes[$fn] = $sz
}

[pscustomobject]@{rows = $rows; total = $total; sizes = $sizes} | ConvertTo-Json -Compress -Depth 3
