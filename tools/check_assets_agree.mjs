#!/usr/bin/env node
/**
 * CI 与仓库对得上吗 —— 把「只在 CI 里才炸」的那几类漂移提前拦下来。
 *
 *     node tools/check_assets_agree.mjs        # 出问题返回 1
 *
 * ── 为什么需要它 ────────────────────────────────────────────────────
 *
 * `.github/workflows/*.yml` 里有一堆**在本地跑不到、只有推上去才会执行**的常量与路径：
 * 调用哪个脚本、缓存哪几个目录、判据用哪个路径。它们和仓库里的真源各写一份
 * （YAML 读不到 JS 常量，只能各写一份）—— 漂了在本地**完全看不出来**。
 *
 * 所以用最笨也最可靠的办法：**把 YAML 当文本读**，逐条核对。不解析 YAML（那要靠
 * 第三方库，而我们不想为这个加依赖），只找 `key: value` 这种行。
 *
 * 查：① workflow 里 `node tools/xxx.mjs` 提到的脚本真的在；② 引用的仓库内路径真的在；
 * ③ 发布目录 `public/` 里没有构建缓存（抓字体的 CSS 缓存必须住在 `src-tauri/target/vss-cache/`，
 * 放 `public/` 会被 Vite 拷进 `dist/`、再被 Tauri 嵌进 exe，而本地完全看不出来）；
 * ④ Settings.tsx 的 JIZURA 署名版本与 `assets.mjs` 钉的一致。
 */

import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'

import * as assets from './assets.mjs'
import {HERE, isMain} from './lib.mjs'
import {RAW_DIR} from './fetch_jizura_fonts.mjs'

const WORKFLOWS = fs.readdirSync(path.join(HERE, '.github/workflows'))
  .filter((f) => f.endsWith('.yml')).sort()
  .map((f) => path.join(HERE, '.github/workflows', f))

// 引用仓库内路径时，这些**前缀**不用查（是运行时/临时目录，不是仓库里的文件）。
// ⚠️ 匹配时要求**路径边界**（等于它、或以它 + "/" 开头）：不然 `src-tauri/target`
//    会把 `src-tauri/target-typo` 也放行 —— 而后者正是「路径写错」的典型样子。
const SKIP_PATH_PREFIXES = ['$', '~', '/', 'dist', 'node_modules', 'src-tauri/target']

function skipped(rel) {
  for (const p of SKIP_PATH_PREFIXES) {
    if (rel === p || rel.startsWith(p + '/')) return true
    if (p.length <= 1 && rel.startsWith(p)) return true
  }
  return false
}

const say = (msg = '') => process.stdout.write(msg + '\n')

/**
 * 文本里提到的 `tools/` 下的脚本要真的在。
 *
 * 两种引用都要认：
 *   · workflow 里的 `node tools/xxx.mjs`（shell 行）；
 *   · `tools/*.mjs` 里的 `tools/xxx.ps1` / `tools/xxx.mjs`（拼在路径里的，比如
 *     `smoke-package.mjs` 调 `msi-file-table.ps1`）—— 只在 workflow 里找会漏掉这一类。
 *
 * ⚠️ 只认「`tools/` + 文件名 + 已知后缀」这种完整形状，别用宽泛的 `tools/[\w./-]+`：
 *    那样会把 `tools/ffmpeg/bin/ffmpeg` 这类**产物落点**也当脚本去查，本地没补齐时必然误报。
 */
function checkScripts(text, name) {
  const bad = []
  /* ⚠️ 先剥掉注释再找引用：注释里为了说明会写出 `tools/xxx.ps1` 这种占位示例，
     照着去 existsSync 必然误报。块注释（JSDoc 的 `*` 续行也在内）与整行 `#` / `//`
     都要剥；行内 `//` 不动 —— 分不清后面是路径还是正文。 */
  const code = text.replace(/\/\*[\s\S]*?\*\//g, '')
    .split('\n').filter((l) => !/^\s*(#|\/\/)/.test(l)).join('\n')
  const seen = new Set(
    [...code.matchAll(/(?:^|[\s'"`(/])(tools\/[\w-]+(?:\/[\w-]+)*\.(?:mjs|ps1|sh))/gm)].map((m) => m[1]),
  )
  for (const rel of [...seen].sort()) {
    if (!fs.existsSync(path.join(HERE, rel))) bad.push(`${name} 引用了不存在的脚本：${rel}`)
  }
  return bad
}

/** workflow 里提到的仓库内路径要真的在。 */
function checkRepoPaths(text, name) {
  const bad = []
  /* ⚠️ 先把**整行注释**去掉再找引用：workflow 里解释「某个路径映射到哪」的注释
     会写出 `../data/xxx` 这种相对路径，那是说明不是引用，照着去 `existsSync`
     必然误报「引用了不存在的路径」。行内 `#` 不动 —— 分不清后面是路径还是
     注释的一部分，宁可保守。 */
  const code = text.split('\n').filter((l) => !/^\s*#/.test(l)).join('\n')
  const candidates = new Set([...code.matchAll(/['"]([^'"\n]+)['"]/g)].map((m) => m[1]))

  // `path: |` 块里是**不带引号**的裸行（actions/cache 的写法），也得查。
  const lines = text.split('\n')
  for (let i = 0; i < lines.length; i++) {
    if (!/^\s*path:\s*\|?\s*$/.test(lines[i])) continue
    const indent = lines[i].length - lines[i].trimStart().length
    for (const nxt of lines.slice(i + 1)) {
      if (/^\s*$/.test(nxt)) continue
      if (nxt.length - nxt.trimStart().length <= indent) break
      candidates.add(nxt.trim())
    }
  }

  for (const rel of [...candidates].sort()) {
    // ⚠️ 路径里不会有空格/空白。这条同时挡掉了「显示给人看的字符串」——
    //    workflow 里有一堆 `echo '  node tools/fetch_tools.mjs'`，它们是提示文案。
    if (rel !== rel.trim() || rel.includes(' ')) continue
    if (skipped(rel)) continue
    if (!rel.includes('/')) continue
    if (rel.startsWith('refs/')) continue
    if (rel.includes('*')) continue
    if (!fs.existsSync(path.join(HERE, rel))) bad.push(`${name} 引用了不存在的路径：${rel}`)
  }
  return bad
}

/** 界面上的 JIZURA 署名要与 `assets.mjs` 钉的版本对得上。 */
function checkJizuraCredits() {
  const tsx = path.join(HERE, 'src/pages/Settings.tsx')
  if (!fs.existsSync(tsx)) return [`找不到 ${tsx}`]
  const found = [...fs.readFileSync(tsx, 'utf8').matchAll(/JIZURA v([0-9][0-9.]*)/g)].map((m) => m[1])
  if (!found.length) return ['Settings.tsx 里找不到 `JIZURA v<版本>` 那句鸣谢']
  const bad = []
  for (const v of new Set(found)) {
    if (v !== assets.UPSTREAM_JIZURA_VERSION) {
      bad.push(`Settings.tsx 里写的是 JIZURA v${v}，而 assets.mjs 钉的是 v${assets.UPSTREAM_JIZURA_VERSION} —— 改一个地方就该改另一个`)
    }
  }
  return bad
}

/** 发布目录（`public/`）里不该有构建缓存。 */
function checkPublishTreeClean() {
  const bad = []
  if (fs.existsSync(path.join(HERE, 'public/vendor/jizura/css-raw'))) {
    bad.push('public/vendor/jizura/ 下有 css-raw/ —— 它是抓字体的缓存，会被拷进 dist/ 再嵌进 exe。应该住在 src-tauri/target/vss-cache/jizura-css-raw/')
  }
  // 直接从脚本里读它的常量，别在检查器里再猜一遍路径
  const pub = path.join(HERE, 'public')
  if (RAW_DIR === pub || RAW_DIR.startsWith(pub + path.sep)) {
    bad.push(`fetch_jizura_fonts.mjs 的 RAW_DIR 在 public/ 下（${RAW_DIR}）—— 缓存会被嵌进二进制，应该放到 src-tauri/target/vss-cache/ 下`)
  }
  return bad
}

function main() {
  if (!WORKFLOWS.length) { say('找不到 .github/workflows/*.yml'); return 1 }

  const problems = assets.selfCheck()
  for (const item of problems) say(`✗ assets.mjs 自检：${item}`)
  if (problems.length) return 1

  let bad = [...checkPublishTreeClean(), ...checkJizuraCredits()]
  for (const wf of WORKFLOWS) {
    const text = fs.readFileSync(wf, 'utf-8')
    const name = path.basename(wf)
    bad = bad.concat(checkScripts(text, name), checkRepoPaths(text, name))
  }
  // `tools/*.mjs` 之间也会互相引用（比如冒烟入口调 `.ps1`）—— 那条路径不在 workflow 里，
  // 上面那圈扫不到。
  for (const f of fs.readdirSync(path.join(HERE, 'tools')).filter((n) => n.endsWith('.mjs')).sort()) {
    const text = fs.readFileSync(path.join(HERE, 'tools', f), 'utf-8')
    bad = bad.concat(checkScripts(text, `tools/${f}`))
  }

  if (bad.length) {
    for (const item of bad) say(`✗ ${item}`)
    say(`\n${bad.length} 处 —— 这些在本地跑不到，只有 CI 才知道。`)
    return 1
  }
  say(`✓ ${WORKFLOWS.length} 个 workflow 与 tools/assets.mjs 对得上`)
  return 0
}

if (isMain(import.meta.url)) process.exit(main())
