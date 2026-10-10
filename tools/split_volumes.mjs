#!/usr/bin/env node
/**
 * 把 `runtime.zip` **按字节切开**成扩展包要的那几卷。
 *
 *     node tools/split_volumes.mjs                      # 资料归档/runtime.zip → 资料归档/volumes/
 *     node tools/split_volumes.mjs --in D:\x.zip --out D:\vols --force
 *     node tools/split_volumes.mjs --check              # 核对已有的一套卷能不能逐字节拼回源包
 *
 * ── 卷名与顺序**不在这里写** ───────────────────────────────────────
 *
 * 从 `assets.mjs` 的 `svsep.runtime` 那条读（`volumes` / `zip`）—— 那是这张表与
 * Rust 的 `RUNTIME_PARTS` 之间唯一的真源。脚本自己再抄一份就会漂。
 *
 * ── 为什么必须是「按字节切」而不是 WinZip/Bandizip 那种「跨卷 zip」 ────
 *
 * 跨卷 zip 是**另一种格式**：中央目录里每个条目记的是「第几卷 + 卷内偏移」，
 * 各卷不能简单首尾相接（接出来的不是合法 zip）。而 GitHub 的 2 GB 资产上限逼着
 * 我们必须分卷，程序那边（`svsep.rs::Volumes`）做的就是「按顺序拼成一个文件再当
 * zip 解」—— 所以切法只有「按字节切」这一种。用错工具的表现：用户下完 4.8 GB
 * 才报 `不是有效的 zip（读不出中央目录）`。
 *
 * ⚠️ 切出来的**每一卷都要传**，包括最后那个小的 `runtime.zip`（它是源包的尾巴，
 * 不是「完整包」）。少一卷 = 谁也拼不出来。
 */

import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'

import * as assets from './assets.mjs'
import {HERE, human, isMain, say} from './lib.mjs'

/** 默认卷大小 0.9 GiB —— 5 个满卷 + 一个小尾巴，正好卡在 GitHub 的 2 GB 资产上限里。 */
const DEFAULT_VOLUME = 966367641

function parseArgs(argv) {
  const a = {in: null, out: null, force: false, check: false, volume: DEFAULT_VOLUME}
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--in') a.in = argv[++i]
    else if (argv[i] === '--out') a.out = argv[++i]
    else if (argv[i] === '--volume') a.volume = Number(argv[++i])
    else if (argv[i] === '--force') a.force = true
    else if (argv[i] === '--check') a.check = true
  }
  return a
}

const asset = assets.byId('svsep.runtime')
const VOLUME_NAMES = asset.volumes
if (!VOLUME_NAMES || VOLUME_NAMES.length < 2) {
  say('assets.mjs 的 svsep.runtime 没写 volumes —— 先补上再切')
  process.exit(1)
}

/** 读一段并写出去，顺手把 sha256 喂进去。 */
function copyChunk(fdIn, fdOut, bytes, hash) {
  const buf = Buffer.allocUnsafe(Math.min(8 * 1024 * 1024, bytes))
  let left = bytes
  while (left > 0) {
    const want = Math.min(buf.length, left)
    const got = fs.readSync(fdIn, buf, 0, want, null)
    if (got === 0) throw new Error('源文件比预期短 —— 盘上的包是不是坏了？')
    const slice = buf.subarray(0, got)
    if (fdOut !== null) fs.writeSync(fdOut, slice)
    hash.update(slice)
    left -= got
  }
}

function main(argv = process.argv.slice(2)) {
  const a = parseArgs(argv)
  const src = path.resolve(a.in || path.join(HERE, '资料归档', asset.zip))
  const outDir = path.resolve(a.out || path.join(HERE, '资料归档', 'volumes'))
  if (!fs.existsSync(src)) {
    say(`找不到源包：${src}`)
    say(`先 node tools/pack_assets.mjs --only svsep.runtime 打一个出来（名字要叫 ${asset.zip}）。`)
    return 1
  }
  const total = fs.statSync(src).size
  say(`源包 ${src}（${human(total)}）`)

  if (a.check) return check(src, outDir, total)

  // 切出来的最后一卷与源包**同名**：落到同一个目录就是自己覆盖自己。
  for (const name of VOLUME_NAMES) {
    if (path.resolve(outDir, name) === src) {
      say(`✗ --out 指到了源包所在目录，最后一卷 ${name} 会盖掉源包本身：${src}`)
      say('  换一个 --out（默认是 资料归档/volumes/）。')
      return 1
    }
  }

  fs.mkdirSync(outDir, {recursive: true})
  const sizes = []
  const fdIn = fs.openSync(src, 'r')
  const whole = crypto.createHash('sha256')
  let written = 0
  try {
    for (const [i, name] of VOLUME_NAMES.entries()) {
      const to = path.join(outDir, name)
      if (fs.existsSync(to) && !a.force) {
        say(`✗ 已存在 ${to} —— 要重切加 --force`)
        return 1
      }
      const bytes = Math.max(0, Math.min(a.volume, total - written))
      if (bytes === 0) {
        say(`✗ 源包只有 ${human(total)}，切不出第 ${i + 1} 卷（${name}）`)
        say('  卷大小给大了，或者 volumes 里的卷数比实际需要的多。')
        return 1
      }
      const h = crypto.createHash('sha256')
      const fdOut = fs.openSync(to, 'w')
      try {
        copyChunk(fdIn, fdOut, bytes, h)
      } finally {
        fs.closeSync(fdOut)
      }
      written += bytes
      sizes.push({name, bytes})
      say(`  ✓ ${name.padEnd(14)} ${String(bytes).padStart(12)}  ${h.digest('hex').slice(0, 16)}…`)
    }
  } finally {
    fs.closeSync(fdIn)
  }

  if (written !== total) {
    say(`✗ 只切出了 ${written} / ${total} 字节：volumes 里那几卷装不下这个包，`)
    say('  得再补一卷（或把 --volume 调大）—— 少一卷谁也拼不出来。')
    return 1
  }

  // 拼回去必须与源包逐字节相同：这是「按字节切」这条约定唯一的判据
  const fdIn2 = fs.openSync(src, 'r')
  try {
    copyChunk(fdIn2, null, total, whole)
  } finally {
    fs.closeSync(fdIn2)
  }
  const want = whole.digest('hex')
  const got = crypto.createHash('sha256')
  for (const {name} of sizes) {
    const fd = fs.openSync(path.join(outDir, name), 'r')
    try {
      copyChunk(fd, null, fs.statSync(path.join(outDir, name)).size, got)
    } finally {
      fs.closeSync(fd)
    }
  }
  const back = got.digest('hex')
  say('')
  if (back !== want) {
    say(`✗ 拼回去的 sha256 与源包不同：\n    源 ${want}\n    拼 ${back}`)
    return 1
  }
  say(`✓ 六卷拼回来与源包逐字节相同（sha256 ${want.slice(0, 16)}…）`)
  say('')
  say(`传到 release 的 \`modules\` 标签，六个都传（在 ${outDir}）。`)
  say('⚠️ 这套卷改了大小或卷数的话，`src-tauri/src/svsep.rs` 的 RUNTIME_PARTS 要跟着改：')
  say('')
  say('const RUNTIME_PARTS: &[Part] = &[')
  for (const {name, bytes} of sizes) {
    say(`    Part { name: ${JSON.stringify(name)}, bytes: ${bytes.toLocaleString('en-US').replaceAll(',', '_')} },`)
  }
  say('];')
  say(`pub const RUNTIME_ZIP_BYTES: u64 = ${total.toLocaleString('en-US').replaceAll(',', '_')};`)
  return 0
}

/** 核对一套已有的卷：逐字节拼回来要和源包一样，顺序按表里的 `volumes`。 */
function check(src, outDir, total) {
  const missing = VOLUME_NAMES.filter((n) => !fs.existsSync(path.join(outDir, n)))
  if (missing.length) {
    say(`✗ 缺 ${missing.length} 卷：${missing.join('、')}`)
    say('  少任何一卷都拼不出 zip —— 包括最后那个小的 runtime.zip（它是尾巴，不是完整包）。')
    return 1
  }
  const want = crypto.createHash('sha256')
  const fdIn = fs.openSync(src, 'r')
  try {
    copyChunk(fdIn, null, total, want)
  } finally {
    fs.closeSync(fdIn)
  }
  const got = crypto.createHash('sha256')
  let summed = 0
  for (const name of VOLUME_NAMES) {
    const p = path.join(outDir, name)
    const size = fs.statSync(p).size
    summed += size
    say(`  · ${name.padEnd(14)} ${String(size).padStart(12)}`)
    const fd = fs.openSync(p, 'r')
    try {
      copyChunk(fd, null, size, got)
    } finally {
      fs.closeSync(fd)
    }
  }
  const a = want.digest('hex')
  const b = got.digest('hex')
  say('')
  if (a !== b) {
    say(`✗ 拼不成源包（合计 ${summed} / 源 ${total} 字节）`)
    say('  这些卷多半不是「按字节切」出来的（跨卷 zip 拼起来不是合法 zip）。')
    say(`    源 ${a}\n    拼 ${b}`)
    return 1
  }
  say(`✓ ${VOLUME_NAMES.length} 卷拼回来与源包逐字节相同`)
  return 0
}

if (isMain(import.meta.url)) {
  try {
    process.exit(main())
  } catch (e) {
    say(`✗ ${e.message}`)
    process.exit(1)
  }
}
