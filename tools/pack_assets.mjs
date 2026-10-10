#!/usr/bin/env node
/**
 * 打包 —— 把 `tools/assets.mjs` 表里那些「我们自己打包」的东西打成 zip。
 *
 *     node tools/pack_assets.mjs                     # 全部（现在只有发布点大件）
 *     node tools/pack_assets.mjs --only release      # game.models, svsep.*
 *     node tools/pack_assets.mjs --only svsep.models # 也可以直接点 id
 *     node tools/pack_assets.mjs --bench             # 只测压缩档位速度
 *     node tools/pack_assets.mjs --out /tmp/zipprobe --force
 *
 * ⚠️ **只打包、不上传，也不分卷**：runtime 那 6 卷由 `tools/split_volumes.mjs`
 * 从这个脚本产出的 `runtime.zip` **按字节切**出来（见那个脚本的头注释：跨卷 zip 不行）。
 *
 * ── 为什么打包只有这一个脚本 ────────────────────────────────────────
 *
 * 「哪个目录 → 哪个 zip → 带不带壳」这个契约**只能有一份**。写反了不会报错，
 * 只会在用户点「工程转换」/「文字 PV」时才现形 —— 所以它由 `assets.mjs` 统一说，
 * 打包与解包都读它，而且每次开跑先过一遍 `assets.selfCheck()`。
 *
 * ⚠️ 压缩档位不是随便定的，实测数据见 `LEVEL_NOTE`。
 */

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import {performance} from 'node:perf_hooks'

import * as assets from './assets.mjs'
import {HERE, human, sha256File, say, walkFiles, isMain} from './lib.mjs'
import {writeZip} from './zip.mjs'

// 实测（本机）：
//
//   | 样本                       | Fastest        | Optimal        | NoCompression |
//   | BS-Roformer-SW.ckpt 667 MB |  56s → 420.7MB | 337s → 403.3MB | 10s → 667.1MB |
//   | torch_cuda.dll 774 MB      |  25s → 606.9MB |  61s → 498.7MB |               |
//
// 模型：ckpt 是张量浮点，压不动（Optimal 多花 280 秒只省 17 MB）—— 但它是用户每次
// 装机都要下的 730 MB，省 6% 就是 44 MB，且总共才 730 MB，几分钟换得来。
// 运行时：CUDA 的 .dll 里塞满零填充与重复符号，Optimal 砍掉 35%。在这个体量上完全划算。
const LEVEL_NOTE = 'Optimal'

const STORED = 0        // 不等同于「不压」—— zip 里 `stored` 才是
const DEFLATE = 9       // zlib 最高档，对应 Python zipfile 的 ZIP_DEFLATED（Optimal）

/**
 * `rel` 是相对某个 piece 的 src 的路径（用 `/` 分隔）。
 *
 * ⚠️ 不含 `/` 的模式**只匹配第一层**，不递归 —— 若按「任意一段路径匹配」来算，
 * `*.py` 会把 LibreSVIP 的插件（几千个 .py）全剔出去，而 zip 照样打得出来、
 * 只在用户点「工程转换」时才发现认不出任何格式。要排深处就写带 `/` 的完整模式。
 */
function excluded(rel, patterns) {
  const top = rel.split('/', 1)[0]
  for (const pat of patterns) {
    if (pat.includes('/')) {
      if (rel === pat || rel.startsWith(pat + '/')) return true
    } else if (pat.startsWith('*.')) {
      if (!rel.includes('/') && top.endsWith(pat.slice(1))) return true
    } else if (top === pat) {
      return true
    }
  }
  return false
}

/** 列出这个 piece 该进包的文件：`{diskPath, name}`。 */
function collect(piece, exclude) {
  const src = path.join(HERE, piece.src)
  const out = []
  if (!fs.existsSync(src)) return out
  if (fs.statSync(src).isFile()) {
    // 单文件子树（game.models 就是这样：白名单 4 个文件，不整目录）
    out.push({diskPath: src, name: piece.prefix + path.basename(src)})
    return out
  }
  for (const p of walkFiles(src).sort()) {
    const rel = path.relative(src, p).split(path.sep).join('/')
    if (excluded(rel, exclude)) continue
    out.push({diskPath: p, name: piece.prefix + rel})
  }
  return out
}

const assetLevelName = (level) => ({[STORED]: 'NoCompression', [DEFLATE]: 'Optimal'}[level] ?? String(level))

/** 打一个包，返回 0=成功 / 1=跳过或失败。 */
async function packOne(asset, outDir, force, level) {
  say('')
  say(`-- ${asset.id} -> ${path.join(outDir, asset.zip)}`)
  say(`   ${asset.label}`)

  let files = []
  for (const pc of asset.pieces) {
    const got = collect(pc, asset.exclude || [])
    if (!got.length) {
      say(`  [跳过] 源不存在或为空：${path.join(HERE, pc.src)}`)
      return 1
    }
    files = files.concat(got)
  }

  const srcBytes = files.reduce((s, f) => s + fs.statSync(f.diskPath).size, 0)
  say(`   源：${files.length.toLocaleString()} 个文件，${human(srcBytes)}`)

  const to = path.join(outDir, asset.zip)
  if (fs.existsSync(to) && !force) {
    say(`  [跳过] 已存在 ${asset.zip}（${human(fs.statSync(to).size)}）—— 要重打加 --force`)
    return 0
  }

  // 条目名不能重复 —— 表自检查了绝大多数情况，但「两棵子树在磁盘上真有同名文件」
  // 只能到这里才知道。撞了就报出来，别静默少文件。
  const seen = new Set()
  for (const {name} of files) {
    if (seen.has(name)) {
      say(`  [错误] 条目名重复：${name}（两棵子树里有同名文件，会互相覆盖）`)
      return 1
    }
    seen.add(name)
  }

  say(`   打包中…（档位 ${assetLevelName(level)}，这一步没有进度条）`)
  const start = performance.now()
  const size = await writeZip(files, to, {level})
  const elapsed = (performance.now() - start) / 1000

  say('')
  say(`  [完成] ${asset.zip}  ${human(size)}  耗时 ${elapsed.toFixed(0)}s` +
    `（${human(elapsed < 1 ? srcBytes : srcBytes / elapsed)}/s）`)
  say(`         SHA-256 ${sha256File(to)}`)
  if (srcBytes) {
    say(`         压缩率 ${Math.round(100 * size / srcBytes)}%  （${human(srcBytes)} -> ${human(size)}）`)
  }

  if (size < asset.min_bytes) {
    say(`  [错误] 产物只有 ${human(size)}，低于这道包的下限 ${human(asset.min_bytes)} —— 是不是打了一半？`)
    return 1
  }
  return 0
}

/** 拿 models 里最大的文件，两个档位各压一遍，比吞吐。 */
async function bench() {
  const src = path.join(HERE, 'data/svsep/models')
  if (!fs.existsSync(src) || !fs.statSync(src).isDirectory()) {
    say(`找不到测速样本目录：${src}`)
    return 1
  }
  const files = walkFiles(src)
  if (!files.length) { say(`测速样本目录是空的：${src}`); return 1 }
  const probe = files.map((f) => ({f, n: fs.statSync(f).size})).sort((a, b) => b.n - a.n)[0].f
  say(`测速样本：${path.basename(probe)}  ${human(fs.statSync(probe).size)}`)

  const tmp = path.join(os.tmpdir(), 'vss-pack-bench.zip')
  for (const [name, level] of [['Fastest', STORED], ['Optimal', DEFLATE]]) {
    fs.rmSync(tmp, {force: true})
    const start = performance.now()
    await writeZip([{diskPath: probe, name: path.basename(probe)}], tmp, {level})
    const elapsed = (performance.now() - start) / 1000
    const size = fs.statSync(tmp).size
    const rate = elapsed ? fs.statSync(probe).size / assets.MB / elapsed : 0
    say(`  ${name.padEnd(14)} ${elapsed.toFixed(1).padStart(7)}s  ${(size / assets.MB).toFixed(1).padStart(8)} MB  (${rate.toFixed(1)} MB/s)`)
    fs.rmSync(tmp, {force: true})
  }
  return 0
}

function parseArgs(argv) {
  const a = {only: 'all', out: null, force: false, bench: false}
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--force') a.force = true
    else if (argv[i] === '--bench') a.bench = true
    else if (argv[i] === '--only') a.only = argv[++i]
    else if (argv[i] === '--out') a.out = argv[++i]
  }
  return a
}

export async function main(argv = process.argv.slice(2)) {
  const a = parseArgs(argv)

  const problems = assets.selfCheck()
  if (problems.length) {
    say('assets.mjs 自检没过，先修表：')
    for (const item of problems) say(`  - ${item}`)
    return 1
  }

  if (a.bench) return bench()

  const want = a.only.split(',').filter(Boolean)
  let todo
  if (want.length === 1 && want[0] === 'all') {
    todo = assets.packable()
  } else if (want.length === 1 && want[0] === 'release') {
    todo = assets.packable().filter((x) => x.host === assets.HOST_RELEASE)
  } else {
    const known = new Set(assets.ARTIFACTS.map((x) => x.id))
    const unknown = want.filter((x) => !known.has(x))
    if (unknown.length) {
      say(`不认识的 id：${unknown}（表里有：${[...known].sort()}）`)
      return 1
    }
    todo = want.map((x) => assets.byId(x))
  }

  const outDir = a.out ? path.resolve(a.out) : path.join(HERE, '资料归档')
  fs.mkdirSync(outDir, {recursive: true})

  let bad = 0
  for (const asset of todo) {
    if (!asset.pieces.length) {
      say(`  [跳过] ${asset.id} 不由我们打包（host=${asset.host}）`)
      continue
    }
    bad += await packOne(asset, outDir, a.force, DEFLATE)
  }

  say('')
  if (bad) { say(`有 ${bad} 项没打成（见上面的 [跳过] / [错误]）。`); return 1 }
  say(`打好了，在 ${outDir}`)
  return 0
}

if (isMain(import.meta.url)) {
  main().then((code) => process.exit(code), (e) => { say(`✗ ${e.message}`); process.exit(1) })
}
