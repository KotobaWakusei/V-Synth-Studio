import {useCallback, useEffect, useRef, useState} from 'react'
import {formatBytes} from './format'

/**
 * 「一键装」—— 音轨分离页与人声转 MIDI 页**共用的一台状态机**。
 *
 * 不做向导、不排队列、也不要用户理解「运行时 / 模型 / 加速包」各是什么 ——
 * 只干一件事：**按顺序把还缺的包一个个下完**，中间不需要再点任何东西。
 *
 * 一次安装就是一条循环：
 *
 * ```
 *   拉一次状态 → 从状态派生「还缺哪几个包」→ 取第一个
 *     ├─ 已经在下了（切页回来 / 上一次没等到收场）→ 不重复触发，跟着它等
 *     └─ 没在跑 → 调这一包的下载命令（后端立刻返回，进度看 download）
 *   每 pollMs 拉一次状态，直到 download.active 变 false：
 *     ├─ download.error 非空 → 停，把**后端那句话原样**交给页面显示
 *     ├─ 这一包的 ready 变真 → 装下一个（回到最上面那一行）
 *     └─ 既没装好也没在跑（用户按了暂停 / 停止）→ 停在这儿，按钮变回「继续安装」
 * ```
 *
 * 三条约束：
 *  1. **离场就收摊** —— `alive` ref 在每一处 `await` 之后都看一眼。不这么做的话，
 *     切页之后循环还接着轮询，回来再点一次就是两条循环同时跑。
 *  2. **同一个下载绝不能被触发两次** —— `busy` ref 挡连点；`download.active` 为真时
 *     **只等、不调 start**：后端对重复触发是直接报错，用户看到的就是一句莫名其妙的红字。
 *  3. **进度不记在内存里** —— 「装到第几步」每次都从**最新状态**重新派生（`plan`），
 *     所以暂停、切页、重启工作站都不会让界面和后端对不上。
 */

/** 一个要装的包。`start` 就是对应的那条下载命令（它立刻返回，进度看 `download`）。 */
export interface InstallStep {
    /** 稳定标识（`'runtime'` / `'models'` / `'dml'`），用来回查这一步装好没有 */
    key: string
    /** 给用户看的名字：「运行时」/「模型」/「显卡加速包」 */
    label: string
    /** 这一步要下多少字节（后端给的）。没有就不显示体积 */
    bytes: number
    /** 这一步装好了吗 —— 判据是后端的状态，不是本地记的 */
    ready: boolean
    start: () => Promise<unknown>
    /**
     * **这台机器用不上这一步**（有 N 卡时的 DirectML 包、白捡来的运行库）。
     *
     * 它和 `ready` 是两件事：`skip` 是「本来就不用装」，`ready` 是「已经装好了」。
     * 分开记是为了按钮文案 —— 否则「一个包都没装、但加速包按策略跳过」会被说成
     * 「继续安装」，而用户明明还没开始装。
     */
    skip?: boolean
    /**
     * 盘上已经有**用户手放的**整套包了（从网盘下下来丢进目录的那种）。
     *
     * 有这个标记时这一步点了**不联网**：后端直接解盘上那份（见 `svsep.rs::manual_sets`）。
     * 按钮文案因此要写成「解压」而不是「下载」—— 说「下载」会让用户以为又在下。
     */
    manual?: boolean
}

/** 后端 `download` 那一块（两个页面同形） */
export interface InstallDownload {
    active: boolean
    kind?: string | null
    /** `'download'` / `'extract'`：解压那一段要换标签、并且不给暂停/停止 */
    stage?: string
    done?: number
    total?: number
    error?: string | null
    resumable?: boolean
    pausedKind?: string | null
    pausedBytes?: number
}

/**
 * 一个包「要下多少」。
 *
 * 优先用后端给的压缩包体积（`zipBytes`）；只有「解开后多大」时退回那个数 ——
 * 宁可估大也不要让按钮上出现一个凭空的数字，**前端一个容量都不写死**。
 */
export function downloadBytes(
    b?: { zipBytes?: number; expectedBytes?: number; extractBytes?: number } | null,
): number {
    return b?.zipBytes || b?.expectedBytes || b?.extractBytes || 0
}

/** 一个包「解开后占多少」。进度条与「磁盘够不够」看它，不是看要下多少。 */
export function extractedBytes(
    b?: { expectedBytes?: number; extractBytes?: number } | null,
): number {
    return b?.expectedBytes || b?.extractBytes || 0
}

/**
 * 「还缺哪几个包」—— 去掉**已经装好的**与**这台机器用不上的**（`skip`）。
 * 安装循环与按钮文案都走它，免得两处对「缺不缺」的理解长歪。
 */
export function missingSteps(steps: InstallStep[]): InstallStep[] {
    return steps.filter((s) => !s.skip && !s.ready)
}

/**
 * 那颗大按钮的文案。`null` = **一个都不缺，别画按钮**（页面那时显示状态摘要）。
 *
 * 一个都没装 → 「安装扩展包（约 X）」；装了一部分 → 「继续安装（还差 约 X）」；
 * **下一步那颗包已经在盘上了**（用户从网盘下好放进去的）→ 「解压」。
 */
export function installLabel(steps: InstallStep[]): string | null {
    const missing = missingSteps(steps)
    if (missing.length === 0) return null
    /* ⚠️ 只看**下一步**那一个：循环是按顺序装的，第一个缺的就是这一次要干的活。
       说「解压」也不算谎 —— 那一步确实一个字节都不下（后端会走手放包那条路）。 */
    if (missing[0].manual) {
        const size = missing[0].bytes > 0 ? `（${formatBytes(missing[0].bytes)}，已在盘上）` : ''
        return `解压${missing[0].label}${size}`
    }
    const bytes = missing.reduce((n, s) => n + (s.bytes || 0), 0)
    const size = bytes > 0 ? formatBytes(bytes) : ''
    const started = steps.some((s) => !s.skip && s.ready)
    if (!started) return size ? `安装扩展包（约 ${size}）` : '安装扩展包'
    return size ? `继续安装（还差 约 ${size}）` : '继续安装'
}

const sleep = (ms: number) => new Promise<void>((r) => window.setTimeout(r, ms))

export function useInstaller<S>(opts: {
    /** 拉一次最新状态 —— 传页面自己那个 `refresh`（顺手把页面状态也更新掉，不必轮询两遍） */
    load: () => Promise<S | null>
    /** 从状态派生「按顺序装哪几个包」。**每轮都重新派生**，别缓存成一份死计划 */
    plan: (st: S) => InstallStep[]
    /** 从状态里取下载进度那一块 */
    download: (st: S) => InstallDownload | null | undefined
    onToast?: (msg: string, tone?: 'ok' | 'err' | 'warn' | 'info') => void
    /** 开装之前问一次（音轨分离拿它确认落点）。回 `false` = 这一次不装 */
    beforeInstall?: () => Promise<boolean>
    /** 循环的轮询间隔（毫秒） */
    pollMs?: number
}) {
    const {onToast, beforeInstall, pollMs = 1750} = opts

    const [installing, setInstalling] = useState(false)
    /** 正在装哪一个包（进度标签用） */
    const [stepKey, setStepKey] = useState<string | null>(null)
    const [stepLabel, setStepLabel] = useState<string | null>(null)
    /** 循环自己撞上的错（起下载失败、读不到状态）。后端那句 `download.error` 由页面显示 */
    const [error, setError] = useState<string | null>(null)

    const alive = useRef(true)
    useEffect(() => {
        alive.current = true
        return () => {
            alive.current = false
        }
    }, [])

    const busy = useRef(false)

    /* 循环是个长跑，而 load / plan / download 每渲染一个新引用 —— 从 ref 里拿最新那份 */
    const live = useRef({load: opts.load, plan: opts.plan, download: opts.download})
    live.current = {load: opts.load, plan: opts.plan, download: opts.download}

    const install = useCallback(() => {
        if (busy.current) return
        busy.current = true
        void (async () => {
            try {
                if (beforeInstall && !(await beforeInstall())) return
                if (!alive.current) return
                setError(null)
                setInstalling(true)

                for (; ;) {
                    const st = await live.current.load()
                    if (!alive.current) return
                    if (!st) {
                        setError('读不到当前状态')
                        return
                    }
                    const next = missingSteps(live.current.plan(st))[0]
                    if (!next) break
                    setStepKey(next.key)
                    setStepLabel(next.label)

                    /* 已经在下了就别再点一次（上一次没等到收场、或者切页回来接着看） */
                    if (!live.current.download(st)?.active) await next.start()

                    /* 等这一包有结论 */
                    for (; ;) {
                        await sleep(pollMs)
                        if (!alive.current) return
                        const now = await live.current.load()
                        if (!alive.current) return
                        if (!now) return
                        const dl = live.current.download(now)
                        /* 后端那句原话由页面显示（它在状态里一直留着，直到下次开始下载） */
                        if (dl?.error) {
                            onToast?.(dl.error, 'err')
                            return
                        }
                        if (!dl?.active) {
                            const after = live.current.plan(now).find((s) => s.key === next.key)
                            /* 装好了 → 下一个包；没装好又没在跑 = 用户按了暂停/停止 → 停在这儿 */
                            if (after?.ready) break
                            onToast?.(`${next.label}还没装完，再点「继续安装」接着下`, 'warn')
                            return
                        }
                    }
                }
                onToast?.('扩展包已装好', 'ok')
            } catch (e) {
                if (alive.current) setError(e instanceof Error ? e.message : String(e))
            } finally {
                busy.current = false
                if (alive.current) {
                    setInstalling(false)
                    setStepKey(null)
                    setStepLabel(null)
                }
            }
        })()
    }, [beforeInstall, onToast, pollMs])

    const dismissError = useCallback(() => setError(null), [])

    /* `dismissError` 留给「错误已经看过了、想手动清掉」的页面；不调也无所谓 ——
       下一次开装时本来就会把它清掉 */
    return {installing, stepKey, stepLabel, error, install, dismissError}
}
