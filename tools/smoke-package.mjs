#!/usr/bin/env node
/**
 * 安装包冒烟：编出来的东西真的装得对 —— 而不是只看编译退出码。
 *
 *     node tools/smoke-package.mjs
 *
 * 判据按平台分叉，入口统一（CI 三个平台都只跑这一句）：
 *   macOS   直接查 `.app` 目录树，最后**真跑**一遍随包二进制
 *   Windows 读 MSI 的 File 表；Node 没有 COM，读表那一步委托 tools/msi-file-table.ps1
 *   Linux   解开 .deb（`ar` 归档里的 data.tar.gz），查 /usr/lib/<name> 那棵树并真跑二进制
 *
 * ⚠️ 三个平台都查不到「前端嵌进去了没有」：`frontendDist = dist/` 被 tauri-codegen
 *    嵌进可执行文件，磁盘与 File 表里都看不见。所以用**体积**兜一道，拦「dist/ 是空的」。
 */

import {spawnSync} from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'

import {PLATFORM} from './assets.mjs'
import {HERE, MB, say, walkFiles} from './lib.mjs'

/** 判据不成立就抛 —— 冒烟只有「全过」和「红」两种结局，收集起来一起报没有意义。 */
function check(ok, msg) {
  if (!ok) throw new Error(msg)
}

/** 跑一个随包二进制，返回 `{status, out}`（stdout + stderr 合流，跟 bash 的 `2>&1` 一致）。 */
function run(bin, args, {timeout = 60_000} = {}) {
  const r = spawnSync(bin, args, {encoding: 'utf8', timeout, maxBuffer: 64 * MB})
  return {status: r.status, out: `${r.stdout || ''}${r.stderr || ''}`}
}

/** 只打印前 `n` 行 —— 诊断信息够定位就行，全量倒进日志会把真正的报错埋掉。 */
const head = (text, n) => text.split('\n').slice(0, n).join('\n')

// ── macOS：查 .app 目录树 ─────────────────────────────────────────────
function smokeMac() {
  const dir = path.join(HERE, 'src-tauri/target/release/bundle/macos')
  const apps = fs.existsSync(dir) ? fs.readdirSync(dir).filter((n) => n.endsWith('.app')) : []
  check(apps.length === 1, `期望恰好一个 .app，实际 ${apps.length} 个（在 ${dir}）`)

  const app = path.join(dir, apps[0])
  const RES = path.join(app, 'Contents/Resources')
  const BIN = path.join(app, 'Contents/MacOS/v-synth-studio')
  say(`app: ${app}`)

  // 出问题时第一眼要看的就是这棵树，先打一层（按路径排序，与 `find | sort` 同序）
  const tree = walkFiles(RES)
    .map((f) => path.relative(RES, f))
    .sort()
    .slice(0, 25)
  for (const rel of tree) say(rel)

  // ① `bundle.resources` 真去装的那几项。少打包是**静默**的，所以逐条查。
  const need = [
    'data/resources.json',
    'data/pinyin.json',
    'tools/ffmpeg/bin/ffmpeg',
    'tools/yt-dlp',
    'tools/libresvip/libresvip-cli/libresvip-cli',
    'tools/onnxruntime/libonnxruntime.dylib',
  ]
  for (const rel of need) check(fs.existsSync(path.join(RES, rel)), `.app 里少了 ${path.join(RES, rel)}`)

  // ② LibreSVIP 的插件目录不能是空壳 —— exe 在、插件没在 = 工程格式互转认不出任何格式，且不报错。
  const plugins = path.join(RES, 'tools/libresvip/libresvip-cli/_internal/libresvip/plugins')
  const n = walkFiles(plugins).length
  say(`LibreSVIP 插件 ${n} 个文件`)
  check(n > 50, `插件只有 ${n} 个文件，明显少打包了`)

  // ③ 随包二进制必须是 arm64。取错平台在编译期看不出来，到用户手里才现形 —— 这层最该拦的一件事。
  const archs = [
    path.join(RES, 'tools/ffmpeg/bin/ffmpeg'),
    path.join(RES, 'tools/yt-dlp'),
    path.join(RES, 'tools/libresvip/libresvip-cli/libresvip-cli'),
    path.join(RES, 'tools/onnxruntime/libonnxruntime.dylib'),
    BIN,
  ]
  for (const b of archs) {
    const r = spawnSync('file', [b], {encoding: 'utf8'})
    check(r.stdout.includes('arm64'), `架构不对：${r.stdout.trim()}`)
  }

  // ④ 可执行文件的体积：前端（含 2335 个字体，约 64 MB）嵌在里面。拦「dist/ 是空的 / 很旧」。
  const sz = fs.statSync(BIN).size
  say(`可执行文件 ${(sz / MB).toFixed(0)} MB`)
  check(sz > 40_000_000, '太小，前端/jizura 没嵌进去')

  // ⑤ **真把它们跑一遍**：架构对不对、动没动态库、包解出来完不完整，只有真执行才知道。
  const FF = path.join(RES, 'tools/ffmpeg/bin/ffmpeg')
  say(head(run(FF, ['-version']).out, 1))
  // 音频页要用的编码器必须都在（LGPL/GPL 构建的差别就在这儿）
  const encs = run(FF, ['-hide_banner', '-encoders']).out
  for (const e of ['libmp3lame', 'libopus', 'libvorbis', 'aac', 'flac']) {
    check(new RegExp(`[\\s]${e}[\\s]`).test(encs), `ffmpeg 里没有 ${e}`)
  }
  say(run(path.join(RES, 'tools/yt-dlp'), ['--version']).out.trim())
  // LibreSVIP 的包里有 4 条符号链接（`_internal/Python` 等），解压器没建出来的话这一句就会失败
  // —— 这正是工程格式互转在 mac 上能不能用的判据。
  say(head(run(path.join(RES, 'tools/libresvip/libresvip-cli/libresvip-cli'), ['plugin', 'detail', 'svp']).out, 3))
}

// ── Linux：解开 .deb 查目录树 ────────────────────────────────────────
//
// deb 是个 `ar` 归档，两个成员：`control.tar.gz` 与 `data.tar.gz`。系统的 `/usr/bin/ar`
// 就够解（不用 dpkg，CI 与开发机上都可能有、也可能没有 dpkg）。`data.tar.gz` 里，
// 程序本体在 `usr/bin/v-synth-studio`、随包资源在 `usr/lib/v-synth-studio/`
// —— 后者正是 Tauri `resource_dir()` 在 Linux 上的解析结果（见 tauri-utils
// `platform.rs::resource_dir_from` 的 linux 分支）。
function smokeLinux() {
  const dir = path.join(HERE, 'src-tauri/target/release/bundle/deb')
  const debs = fs.existsSync(dir) ? fs.readdirSync(dir).filter((n) => n.endsWith('.deb')) : []
  check(debs.length === 1, `期望恰好一个 deb，实际 ${debs.length} 个（在 ${dir}）`)
  const deb = path.join(dir, debs[0])
  say(`deb: ${deb}   ${(fs.statSync(deb).size / MB).toFixed(1)} MB`)

  // 解开：ar 抽成员 → 只取 data.tar.gz → 解到临时目录。
  const work = fs.mkdtempSync(path.join(os.tmpdir(), 'vss-smoke-'))
  try {
    const ar = spawnSync('ar', ['x', deb], {cwd: work, encoding: 'utf8', maxBuffer: 64 * MB})
    check(ar.status === 0, `ar 解 deb 失败：${ar.error || ar.stderr}`)
    const dataTgz = path.join(work, 'data.tar.gz')
    check(fs.existsSync(dataTgz), 'deb 里没有 data.tar.gz —— 打包布局变了？')
    const tar = spawnSync('tar', ['xzf', 'data.tar.gz', '-C', work], {cwd: work, encoding: 'utf8', maxBuffer: 64 * MB})
    check(tar.status === 0, `解 data.tar.gz 失败：${tar.error || tar.stderr}`)

    // 顶层就是 deb 的根（`usr/`、`DEBIAN` 不在这儿 —— 那在 control 成员里）。
    const LIB = path.join(work, 'usr/lib/v-synth-studio')
    const BIN = path.join(work, 'usr/bin/v-synth-studio')

    // 出问题时第一眼要看的就是这棵树，先打一层
    for (const rel of walkFiles(LIB).map((f) => path.relative(work, f)).sort().slice(0, 25)) say(rel)

    // ① `bundle.resources` 真去装的那几项 + 程序本体。
    const need = [
      'usr/bin/v-synth-studio',
      'usr/lib/v-synth-studio/data/resources.json',
      'usr/lib/v-synth-studio/data/pinyin.json',
      'usr/lib/v-synth-studio/tools/ffmpeg/bin/ffmpeg',
      'usr/lib/v-synth-studio/tools/yt-dlp',
      'usr/lib/v-synth-studio/tools/libresvip/libresvip-cli/libresvip-cli',
      'usr/lib/v-synth-studio/tools/onnxruntime/libonnxruntime.so',
    ]
    for (const rel of need) check(fs.existsSync(path.join(work, rel)), `deb 里少了 ${rel}`)

    // ② LibreSVIP 的插件目录不能是空壳（exe 在、插件没在 = 工程格式互转认不出任何格式，且不报错）。
    const plugins = path.join(LIB, 'tools/libresvip/libresvip-cli/_internal/libresvip/plugins')
    const n = walkFiles(plugins).length
    say(`LibreSVIP 插件 ${n} 个文件`)
    check(n > 50, `插件只有 ${n} 个文件，明显少打包了`)

    // ③ 随包二进制必须是 x86_64。取错架构在编译期看不出来，到用户手里才现形。
    for (const b of [BIN, path.join(LIB, 'tools/ffmpeg/bin/ffmpeg'), path.join(LIB, 'tools/yt-dlp'),
                     path.join(LIB, 'tools/libresvip/libresvip-cli/libresvip-cli'),
                     path.join(LIB, 'tools/onnxruntime/libonnxruntime.so')]) {
      const r = spawnSync('file', [b], {encoding: 'utf8'})
      check(/x86-64|x86_64/.test(r.stdout), `架构不对：${r.stdout.trim()}`)
    }

    // ④ 可执行文件的体积：前端（含 2335 个字体，约 64 MB）嵌在里面。拦「dist/ 是空的 / 很旧」。
    const sz = fs.statSync(BIN).size
    say(`可执行文件 ${(sz / MB).toFixed(0)} MB`)
    check(sz > 40_000_000, '太小，前端/jizura 没嵌进去')

    // ⑤ **真把它们跑一遍**：架构对不对、动没动态库、解出来完不完整，只有真执行才知道。
    const FF = path.join(LIB, 'tools/ffmpeg/bin/ffmpeg')
    say(head(run(FF, ['-version']).out, 1))
    const encs = run(FF, ['-hide_banner', '-encoders']).out
    for (const e of ['libmp3lame', 'libopus', 'libvorbis', 'aac', 'flac']) {
      check(new RegExp(`[\\s]${e}[\\s]`).test(encs), `ffmpeg 里没有 ${e}`)
    }
    say(run(path.join(LIB, 'tools/yt-dlp'), ['--version']).out.trim())
    // LibreSVIP 是 Python 打包出来的，真跑一次才验得出 `_internal/` 解完整了没有。
    const rpc = run(path.join(LIB, 'tools/libresvip/libresvip-cli/libresvip-cli'), ['plugin', 'detail', 'svp'])
    say(head(rpc.out, 3))
    check(rpc.status === 0, 'libresvip-cli 跑不起来 —— _internal/ 没解完整？')
  } finally {
    fs.rmSync(work, {recursive: true, force: true})
  }
}

// ── Windows：读 MSI 的 File 表 ────────────────────────────────────────
function smokeWin() {
  const dir = path.join(HERE, 'src-tauri/target/release/bundle')
  const glob = (sub, ext) => {
    const d = path.join(dir, sub)
    return fs.existsSync(d) ? fs.readdirSync(d).filter((n) => n.endsWith(ext)).map((n) => path.join(d, n)) : []
  }

  const msi = glob('msi', '.msi')
  check(msi.length === 1, `期望恰好一个 MSI，实际 ${msi.length} 个`)
  say(`MSI: ${msi[0]}   ${(fs.statSync(msi[0]).size / MB).toFixed(1)} MB（压缩后）`)

  // exe 安装程序：与 MSI 同一个程序的另一种装法。只查「恰好一个 + 体积下限」——
  // 它的内容与 MSI 同源，逐件核对在 MSI 那边已经做了。
  const setup = glob('nsis', '.exe')
  check(setup.length === 1, `期望恰好一个 exe 安装程序，实际 ${setup.length} 个`)
  const setupMb = fs.statSync(setup[0]).size / MB
  say(`exe 安装程序：${path.basename(setup[0])}   ${setupMb.toFixed(1)} MB`)
  check(setupMb >= 100, `exe 安装程序只有 ${setupMb.toFixed(1)} MB，明显少打包了`)

  const ps = spawnSync('powershell.exe',
    ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', path.join(HERE, 'tools/msi-file-table.ps1'), '-Msi', msi[0]],
    {encoding: 'utf8', maxBuffer: 64 * MB})
  check(ps.status === 0, `读 MSI 的 File 表失败：${ps.error || ps.stderr || ps.stdout}`)
  // 去掉 BOM / 首尾空白再解 —— 只认 JSON 本体，不依赖 PowerShell 那边一点多余输出都没有。
  let table
  try {
    table = JSON.parse(ps.stdout.replace(/^\uFEFF/, '').trim())
  } catch (e) {
    throw new Error(`读 MSI 的 File 表回来的不是 JSON：${e.message}\n${head(ps.stdout, 5)}`)
  }
  const {rows, total, sizes} = table
  say(`File 表：${rows} 个文件（${Object.keys(sizes).length} 个不同文件名），${(total / MB).toFixed(1)} MB（未压缩）`)

  // ① `bundle.resources` 真去装的那几个 + 程序本体。
  //    **不要**加 `index.html` / `ffprobe.exe` —— 前者走 `frontendDist` 嵌在 exe 里、
  //    不在磁盘上，后者不由产物表产出。加了必然误报。
  const need = ['v-synth-studio.exe', 'resources.json', 'pinyin.json',
                'ffmpeg.exe', 'yt-dlp.exe', 'libresvip-cli.exe', 'onnxruntime.dll']
  const miss = need.filter((n) => !(n in sizes))
  check(miss.length === 0, `MSI 里少了：${miss.join('、')} —— bundle.resources 少打包是静默的`)

  // ② 个数与总量下限（拦「映射少写一条」这种大错，不是「少了一个插件」）。
  //    ⚠️ **「多少文件」必须数行，不能数不同文件名**：libresvip 那棵 Python 树里
  //    `__init__.py` 出现 51 次、`options.py` 45 次，数名字会少算 299 个（1096 vs 1395）。
  check(rows >= 1150, `MSI 里只有 ${rows} 个文件，明显少打包了（实测 ≈1395）`)
  check(total >= 150 * MB, `MSI 内容只有 ${(total / MB).toFixed(1)} MB，明显少打包了（实测 ≈262MB）`)

  // ③ exe 的体积：前端（含 2335 个字体，约 64MB）嵌在里面。
  //    40MB 这条拦的是「dist/ 是空的/很旧」—— 那种情况下 exe 只有正常的一半。
  const exeMb = sizes['v-synth-studio.exe'] / MB
  say(`程序本体 ${exeMb.toFixed(1)} MB（前端与 jizura 嵌在里面）`)
  check(exeMb >= 40, `程序本体只有 ${exeMb.toFixed(1)} MB —— 嵌入的前端/jizura 太小了。` +
    '「dist/」是在 cargo build 时被 tauri-codegen 嵌进 exe 的，只有它为空或很旧才会这么小。' +
    '检查 pnpm exec tauri build 有没有正常跑它的 beforeBuildCommand（pnpm build）。')
}

try {
  if (PLATFORM === 'macos') smokeMac()
  else if (PLATFORM === 'windows') smokeWin()
  else if (PLATFORM === 'linux') smokeLinux()
  else throw new Error(`这个脚本只覆盖 macOS / Windows / Linux，当前平台是 ${PLATFORM}`)
} catch (e) {
  say(`✗ ${e.message}`)
  process.exit(1)
}
say('布局、架构与总量都对 ✓')
