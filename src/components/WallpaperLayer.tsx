/**
 * 背景壁纸层 —— 界面最底下那一层，`body::before`（静态背景图）的替代品。
 *
 * ⚠️ **它必须不透明**：玻璃材质靠 `backdrop-filter` 采样「身后有什么」，这一层一透明，
 * 整个界面的玻璃会**静默失效**。所以底色先铺一个 `--lg-bg-grouped`，再往上放壁纸。
 *
 * ⚠️ **场景壁纸跑在 sandbox iframe 里**（`allow-scripts`，**不给** `allow-same-origin`）：
 * 场景里的 SceneScript 是别人的代码，webwallgl 的脚本沙箱只是 JS 语义隔离、不是安全
 * 边界。不透明源之后它连父窗口都拿不到，自然也碰不到 `window.__TAURI_INTERNALS__`。
 *
 * ⚠️ **视频壁纸要先放行目录、再给它 `src`**（下面 `mediaReady`）：asset 协议的放行名单
 * 是**进程内**的，重启工作站就没了，而 `<video>` 拿到 403 之后不会自己重试 —— 表现就是
 * 「重启后视频壁纸一片黑，切页/重扫才好」。
 *
 * ⚠️ **video / image 不走 webwallgl**：它的媒体路径要 `texImage2D(视频帧)`，而壁纸文件
 * 走 asset 协议（跨源）→ 跨源视频会污染 WebGL 纹理、直接抛安全错。原生 `<video>` 没有
 * 这个问题，观感一样（视频壁纸本来就是一段视频铺满）。
 *
 * ⚠️ **渲染失败要说话**：这个库还年轻，场景里用到的特性（自定义着色器、模型、脚本）
 * 不是都支持。失败时**退到预览图并说明原因** —— 静默退回的话，用户只知道「壁纸没动」。
 */

import {useEffect, useRef, useState} from 'react'

import {api} from '@/lib/api'
import {getConfig, onConfigChange} from '@/lib/config'
import {fileUrl} from '@/lib/ipc'
import {loadPkg, planWallpaper, useWeScan, type WallpaperPlan} from '@/lib/wallpaper'

import './WallpaperLayer.css'

/** 宿主页（`public/vendor/wallpaper/index.html`）。 */
const HOST_PAGE = '/vendor/wallpaper/index.html'

/**
 * 场景包的缓存键（webwallgl 的 `source.key`）。
 *
 * ⚠️ **固定串，不是壁纸 id**：换壁纸时整个 iframe 被 `key` 换掉（见下面的 `key={plan.pkg}`），
 * 页面级缓存跟着一起没了，所以这个键不需要区分是哪一张 —— 而它一旦带上 id，`mount`
 * 就得多传一次 `plan.item.id`，那条路没有任何别的东西读它。
 */
const SCENE_KEY = 'scene'

export function WallpaperLayer() {
    const {scan} = useWeScan()
    const [cfg, setCfg] = useState(() => getConfig())
    const [error, setError] = useState<string | null>(null)
    const [noticeClosed, setNoticeClosed] = useState(false)
    /** 已经放行完 asset 协议、可以挂 `src` 的那一张（`planKey`；空 = 还没好） */
    const [mediaReady, setMediaReady] = useState('')
    const frameRef = useRef<HTMLIFrameElement>(null)
    const videoRef = useRef<HTMLVideoElement>(null)
    /** 这一张视频已经重试过一次了（`onError` 里的第二次机会） */
    const retried = useRef('')
    /** 现在该装哪个场景包（`''` = 这一张不是场景）。**由 effect 维护、被 `boot` 读** */
    const sceneRef = useRef('')
    /** 已经喂进去的包（`mount` 幂等：`boot` 可能来两次） */
    const mounted = useRef<Set<string>>(new Set())
    /** 问宿主页「你起来了吗」。宿主页收到就回一条 `boot`（见 `index.html` 的协议） */
    const askFrame = () => {
        frameRef.current?.contentWindow?.postMessage({type: 'hello'}, '*')
    }

    /* 配置一变就重算（设置页点一下立刻生效）。订阅是模块级的、这个组件只挂一次，
       所以不退订。 */
    useEffect(() => {
        onConfigChange(() => setCfg(getConfig()))
    }, [])

    const plan: WallpaperPlan = planWallpaper(String(cfg['wallpaper'] ?? ''), scan)
    const paused = Boolean(cfg['wallpaperPaused'])
    /** 只跟「哪一张」有关：换壁纸才重挂，暂停/主题变化不重挂。 */
    const planKey = plan.kind === 'none' ? '' : `${plan.kind}:${plan.item.id}`
    /** 这一张要装的场景包（空 = 不是场景）。当依赖用，**别在依赖里写三元表达式** */
    const scenePkg = plan.kind === 'scene' ? (plan.pkg ?? '') : ''

    const fail = (reason: string) => {
        setError(reason)
        setNoticeClosed(false)
        console.warn('[wallpaper]', reason)
    }

    /** 把一个场景包喂进 iframe（同一个包不重复喂）。 */
    const feedScene = async (pkgPath: string) => {
        const frame = frameRef.current
        if (!frame || !pkgPath || mounted.current.has(pkgPath)) return
        mounted.current.add(pkgPath)
        try {
            const pkg = await loadPkg(pkgPath)
            /* 30fps：这是背景层，上面还压着玻璃。上游实测帧率上限是省 CPU 最有效的
               那一档（场景壁纸稳态 -25~38%），而 30fps 的观感对壁纸足够。 */
            /* ⚠️ 字节走 `transfer` 转移所有权（几十 MB 的包别复制）。 */
            frame.contentWindow?.postMessage({type: 'mount', pkg, key: SCENE_KEY, fps: 30}, '*', [pkg])
        } catch (e) {
            mounted.current.delete(pkgPath)
            fail(`场景包没喂进去：${e instanceof Error ? e.message : String(e)}`)
        }
    }

    /* `html[data-wallpaper]` 一在，`body::before` 的静态图就被关掉（见 css）——
       否则两张图叠着。退回静态图时把标记摘掉。 */
    useEffect(() => {
        const root = document.documentElement
        setError(null)
        if (!planKey) delete root.dataset.wallpaper
        else root.dataset.wallpaper = 'on'
        return () => {
            delete root.dataset.wallpaper
        }
    }, [planKey])

    /* 媒体类要先把这个目录放行给 asset 协议，否则 `<video>` 静默 403（控制台里才有）。
       ⚠️ 放行**完**才让 `mediaReady` 落地：并行的话 `<video>` 会在放行生效之前就发请求。 */
    useEffect(() => {
        if (plan.kind !== 'video' && plan.kind !== 'image') return
        let alive = true
        void api
            .allowPath(plan.item.dir)
            .catch(() => undefined)
            .then(() => {
                if (alive) setMediaReady(planKey)
            })
        return () => {
            alive = false
        }
    }, [planKey])

    /* 宿主页的消息：`boot` 一到就把当前该装的包装上。
       ⚠️ **监听只注册一次**（依赖为空）：重启那次 `boot` 之所以会丢，就是因为「谁先到」；
       把它挂成常驻的、再配上主动 `hello`，两头都不靠时序。 */
    useEffect(() => {
        const onMsg = (ev: MessageEvent) => {
            const frame = frameRef.current
            if (!frame || ev.source !== frame.contentWindow) return
            const d = (ev.data ?? {}) as {type?: string; message?: string}
            if (d.type === 'boot') {
                void feedScene(sceneRef.current)
                return
            }
            if (d.type === 'error') fail(`场景渲染失败：${d.message || '未知原因'}`)
            if (d.type === 'diagnostic') console.warn('[wallpaper] 诊断：', d.message)
        }
        window.addEventListener('message', onMsg)
        return () => {
            window.removeEventListener('message', onMsg)
            frameRef.current?.contentWindow?.postMessage({type: 'destroy'}, '*')
        }
    }, [])

    /* 换壁纸：**先问一次**宿主页，不指望它开场那条 `boot` 被听见。
       ⚠️ iframe 是全新挂上来的（`key={plan.pkg}`），它的脚本一定晚于这个 effect 执行，
       所以这条 `hello` 只会被它之后的 `boot` 回应 —— 这里不靠运气。 */
    useEffect(() => {
        if (plan.kind !== 'scene') {
            sceneRef.current = ''
            return
        }
        sceneRef.current = scenePkg
        askFrame()
    }, [scenePkg])

    /* 暂停/恢复：设置里的开关、或者窗口被切到后台（省电，也免得玻璃每帧重算）。 */
    useEffect(() => {
        const apply = () => {
            const stop = paused || document.hidden
            if (plan.kind === 'scene') {
                frameRef.current?.contentWindow?.postMessage({type: stop ? 'pause' : 'resume'}, '*')
            } else {
                const v = videoRef.current
                if (!v) return
                if (stop) v.pause()
                else void v.play().catch(() => undefined)
            }
        }
        apply()
        document.addEventListener('visibilitychange', apply)
        return () => document.removeEventListener('visibilitychange', apply)
    }, [paused, planKey])

    if (plan.kind === 'none') return null

    const failed = error !== null
    const preview = plan.item.preview
    const showMedia = (plan.kind === 'video' || plan.kind === 'image') && mediaReady === planKey

    return (
        <>
            <div className="wallpaper" aria-hidden="true">
                {failed ? (
                    /* 失败了就显示这张壁纸的预览图（WE 每张都带 preview.gif/jpg）。
                       连预览都没有时什么都不画 —— 底下就是静态背景图那层。 */
                    preview ? <img className="wallpaper-media" src={fileUrl(preview)} alt=""/> : null
                ) : (
                    <>
                        {plan.kind === 'scene' && (
                            /* `sandbox` 只给 allow-scripts：不透明源，碰不到宿主的 IPC。
                               `key` 跟着场景包走 —— 换壁纸时整页重载，省掉「在旧场景上
                               原地换包」那条路。 */
                            <iframe
                                key={plan.pkg}
                                ref={frameRef}
                                className="wallpaper-frame"
                                src={HOST_PAGE}
                                sandbox="allow-scripts"
                                title=""
                            />
                        )}
                        {plan.kind === 'video' && showMedia && (
                            <video
                                key={plan.src}
                                ref={videoRef}
                                className="wallpaper-media"
                                src={fileUrl(plan.src)}
                                autoPlay
                                muted
                                loop
                                playsInline
                                /* ⚠️ 视频解不了是**常态**，别让它变成一块黑：`canPlayType`
                                   会说 HEVC「probably」，真放起来却是 `MediaError 4`
                                   （本机那张 STUDY WITH MIKU 就是 HEVC，没有系统的
                                   HEVC 扩展就播不了）。
                                   ⚠️ 只有**同一个 src 失败两次**才认输：第一次换一个带查询串的
                                   地址逼 WebView 重新取（已经 failed 的资源上直接调
                                   `load()` 不生效），顺带把「重启后 asset 放行时序」那一线
                                   的窗口补掉。 */
                                onError={(e) => {
                                    const el = e.target as HTMLVideoElement
                                    if (retried.current !== plan.src) {
                                        retried.current = plan.src
                                        const again = new URL(el.src)
                                        again.searchParams.set('retry', '1')
                                        el.src = again.toString()
                                        void el.play().catch(() => undefined)
                                        return
                                    }
                                    const err = el.error
                                    fail(
                                        `视频解不了（MediaError ${err?.code ?? '?'}）：` +
                                            '这个编码 WebView2 放不出来',
                                    )
                                }}
                            />
                        )}
                        {plan.kind === 'image' && showMedia && (
                            <img key={plan.src} className="wallpaper-media" src={fileUrl(plan.src)} alt=""/>
                        )}
                    </>
                )}
            </div>
            {failed && !noticeClosed && error && (
                <WallpaperNotice title={plan.item.title} reason={error} onClose={() => setNoticeClosed(true)}/>
            )}
        </>
    )
}

/**
 * 渲染失败时的说明条。
 *
 * ⚠️ 它是 `.wallpaper` 的**兄弟**而不是子节点：那一层是 `pointer-events: none`，
 * 放进去的话「知道了」点不动。所以这里单独 fixed 定位、自己开 `pointer-events`。
 */
function WallpaperNotice({title, reason, onClose}: {title: string; reason: string; onClose: () => void}) {
    return (
        <div className="wallpaper-notice" role="status">
            <div className="wallpaper-notice-text">
                <strong>「{title}」没能渲染</strong>
                <span>已退回这张壁纸的预览图。原因：{reason.slice(0, 300)}</span>
            </div>
            <button type="button" onClick={onClose}>
                知道了
            </button>
        </div>
    )
}
