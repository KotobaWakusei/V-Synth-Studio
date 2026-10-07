/**
 * 一次性验收：把 `public/vendor/wallpaper/index.html` 里那段**真脚本**抠出来，
 * 在一个只有 `addEventListener` / `parent.postMessage` 的假环境里跑一遍，
 * 验证「父窗口问 hello → 宿主页回 boot」这条握手真的成立。
 *
 * 这条握手是「重启工作站后动态壁纸不播放」那个 bug 的修复本体：宿主页开场那条
 * `boot` 常常在父窗口的监听器注册之前就发出去了，所以必须**能被再问一次**。
 *
 *     node tools/check_wallpaper_handshake.mjs
 */

import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import vm from 'node:vm'

import {HERE, say} from './lib.mjs'

const PAGE = path.join(HERE, 'public', 'vendor', 'wallpaper', 'index.html')
const html = fs.readFileSync(PAGE, 'utf8')

/* 只取 `(() => { … })()` 那一段（页面里最后一块 <script>）。
   ⚠️ 不引 HTML 解析器：这个页面是我们自己的，脚本块的形状是固定的；用正则抠出来
   跑，验的是**脚本本身**，不是「HTML 里有没有这段文字」。 */
const blocks = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1])
const main = blocks.find((b) => b.includes("d.type === 'mount'"))
if (!main) {
  say('从 index.html 里没找到主脚本块 —— 页面的结构变了？')
  process.exit(1)
}

/** 宿主页往父窗口发出去的消息（就是我们要断言的东西）。 */
const posted = []
const listeners = []
const box = {}

const sandbox = {
  console,
  Object,
  String,
  document: {getElementById: () => box},
  addEventListener: (type, fn) => listeners.push([type, fn]),
  parent: {postMessage: (m) => posted.push(m)},
  /* 宿主页要的全局是 `WebWallGL`（真实页面用 `<script src>` 挂上去的），
     而它调的是 `WebWallGL.mount` / `WebWallGL.bytesSource`。 */
  WebWallGL: {
    mount: async () => ({info: {ok: true}, stats: {fps: 30}}),
    bytesSource: () => ({}),
  },
}
sandbox.window = sandbox

vm.createContext(sandbox)
vm.runInContext(main, sandbox)

/* 把一条消息投给宿主页（模拟父窗口 postMessage 过来的东西）。 */
const deliver = (data) =>
  Promise.all(listeners.filter(([t]) => t === 'message').map(([, fn]) => fn({data})))

const problems = []

// ① 开场那条 boot：老版本也有它，不能因为加了 hello 就丢
if (!posted.some((m) => m.type === 'boot')) {
  problems.push('开场没有主动 post 一条 boot')
}

// ② ★ 父窗口问 hello → 必须回一条 boot（这才是修复：开场那条常常没人听见）
const before = posted.length
await deliver({type: 'hello'})
const answered = posted.slice(before).some((m) => m.type === 'boot')
if (!answered) {
  problems.push('收到 hello 之后没有回 boot —— 重启后那条开场 boot 丢了就再也接不上')
}

// ③ 认不出的消息不该炸，也不该乱回
const afterHello = posted.length
await deliver({type: 'nonsense'})
if (posted.length !== afterHello) {
  problems.push('认不出的消息也回了东西')
}

// ④ mount 仍然要能走通（回到 hello 那条路不能把装配拆坏）
const beforeMount = posted.length
await deliver({type: 'mount', pkg: new ArrayBuffer(8), key: 'scene', fps: 30})
const kinds = posted.slice(beforeMount).map((m) => m.type)
if (!kinds.includes('ready')) {
  problems.push(`mount 没有走到 ready（收到的是 ${JSON.stringify(kinds)}）`)
}

if (problems.length) {
  for (const p of problems) say(`✗ ${p}`)
  process.exit(1)
}
say(`✓ 握手成立：开场有 boot、hello 能再要到一条 boot、mount 仍能走到 ready`)
