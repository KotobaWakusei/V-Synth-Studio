import {api, MANUAL_PACKS_URL} from '@/lib/api'
import './ManualPacks.css'

/**
 * 「下得慢可以自己去 123 云盘下」那一行 —— 音轨分离页与人声转 MIDI 页**共用一份**。
 *
 * 三件事必须一起说清，少一件用户就会做错：
 *  1. **链接**（蓝字、点了交给**系统默认浏览器**）；
 *  2. **不用解压** —— 网盘上那份是压缩包，丢进去就行，界面会把按钮换成「解压」；
 *  3. **放到哪** —— `dir` 是后端给的**当前**扩展包根，用户在设置里改了它跟着变。
 *
 * ⚠️ **不能写成 `<a href>`**：窗口里加载的是 `http://tauri.localhost`，真链接会把整个
 * 界面换成那个网页（WebView 里没有后退键可回来）。所以是「长成链接的 `<button>`」，
 * 与 `components/Credit.tsx` 的 `Upstream` 同一套路子。
 */
export function ManualPacks({
                                dir,
                                what,
                                onToast,
                            }: {
    /** 扩展包根（后端的 `extDir`）。拿不到就不显示路径那一句 */
    dir?: string
    /** 「运行时那 6 卷和 models.zip」这种，说清要放哪几个文件 */
    what: string
    onToast?: (msg: string, tone?: 'ok' | 'err' | 'warn' | 'info') => void
}) {
    return (
        <p className="manual-packs">
            下得慢可以自己去 123 云盘下载：
            <button
                type="button"
                className="manual-packs-link"
                title={`在系统浏览器里打开：${MANUAL_PACKS_URL}`}
                onClick={() => {
                    api.fsOpen({url: MANUAL_PACKS_URL}).catch((e: unknown) => {
                        const msg = e instanceof Error ? e.message : String(e)
                        onToast?.(`打不开浏览器：${msg}`, 'err')
                    })
                }}
            >
                {MANUAL_PACKS_URL}
            </button>
            <br/>
            下载完<strong>不用解压</strong>，把{what}直接放到{' '}
            {dir ? <code className="manual-packs-dir">{dir}</code> : '扩展包目录'} 下就行 ——
            放好之后这颗按钮会变成「解压」，点了就地解开，不会再联网。
        </p>
    )
}
