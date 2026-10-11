/**
 * 背景壁纸：「配置里选了什么」+「库里有什么」→「这一层该画什么」。
 *
 * 数据全部来自 Rust 侧对**用户自己的** Wallpaper Engine 库的只读扫描
 * （`wallpaper_scan`），本模块不联网、不缓存文件。
 *
 * ⚠️ 三种壁纸走两条完全不同的路（见 `WallpaperLayer.tsx`）：
 *   - `scene`：`scene.pkg` 的字节交给 webwallgl，在**沙箱 iframe** 里渲染；
 *   - `video` / `image`：直接用 `<video>` / `<img>` 走 asset 协议。
 *   别把 video 也塞给 webwallgl —— 它的媒体路径要 `texImage2D` 上传视频帧，
 *   而 asset 协议是跨源，跨源视频会**污染 WebGL 纹理**（直接抛安全错）。
 */

import {useCallback, useEffect, useState} from 'react'

import {api, type WeItem, type WeScan} from './api'

/* 类型本体在 `api.ts`（回包的形状由那边定义），这里只做转出，别处不用两边找。 */
export type {WeItem, WeScan}

/** 这一层要画什么。`none` = 回到静态背景图。 */
export type WallpaperPlan =
    | { kind: 'none' }
    | { kind: 'scene'; item: WeItem; pkg: string }
    | { kind: 'video' | 'image'; item: WeItem; src: string }

const VIDEO_EXT = ['.mp4', '.webm', '.mkv', '.mov', '.m4v', '.avi']

/**
 * 配置里的 `wallpaper` 字符串 + 扫描结果 → 这一层要画什么。
 *
 * 认不出的值、选中的那张已经不在库里（订阅被删/盘被拔）都回 `none` ——
 * 退回静态背景图，而不是留一块黑。
 */
export function planWallpaper(selected: string, scan: WeScan | null): WallpaperPlan {
    const sel = (selected || '').trim()
    if (!sel || !scan) return {kind: 'none'}
    const item = sel === 'we:current' ? scan.current : scan.items.find((i) => `we:${i.id}` === sel) ?? null
    if (!item) return {kind: 'none'}

    if ((item.type || '').toLowerCase() === 'scene') {
        return item.pkg ? {kind: 'scene', item, pkg: item.pkg} : {kind: 'none'}
    }
    if (item.media) {
        const lower = item.media.toLowerCase()
        return {kind: VIDEO_EXT.some((e) => lower.endsWith(e)) ? 'video' : 'image', item, src: item.media}
    }
    /* 其余类型（web / application）暂时没有可播的东西：退到预览图 ——
       至少画的是「这张壁纸」，而不是一块纯色。 */
    return item.preview ? {kind: 'image', item, src: item.preview} : {kind: 'none'}
}

/**
 * 扫一遍壁纸库。**只在需要时调**（设置页打开、或用户切换了选择）——
 * 扫的是磁盘上几十个 `project.json`，几毫秒，但没必要每次进页面都扫。
 */
export function useWeScan() {
    const [scan, setScan] = useState<WeScan | null>(null)
    const [error, setError] = useState<string | null>(null)
    const [busy, setBusy] = useState(false)

    const reload = useCallback(async () => {
        setBusy(true)
        try {
            setScan((await api.wallpaperScan()) as unknown as WeScan)
            setError(null)
        } catch (e) {
            setError(e instanceof Error ? e.message : String(e))
        } finally {
            setBusy(false)
        }
    }, [])

    useEffect(() => {
        void reload()
    }, [reload])

    return {scan, error, busy, reload}
}

/**
 * 场景包的字节。
 *
 * ⚠️ 走的是 `wallpaper_pkg`（Rust 回 `tauri::ipc::Response`，即原始字节）——
 * 不能用 `read_bytes`，那边回的是 `{bytes:[1,2,3…]}`，几十 MB 过 JSON 会卡死窗口。
 * 两种回包形状都兜一下：不同 Tauri 版本给 `ArrayBuffer` 还是 `Uint8Array` 不一定。
 */
export async function loadPkg(path: string): Promise<ArrayBuffer> {
    const raw = (await api.wallpaperPkg(path)) as unknown
    if (raw instanceof ArrayBuffer) return raw
    if (ArrayBuffer.isView(raw as ArrayBufferView)) {
        const v = raw as Uint8Array
        return v.buffer.slice(v.byteOffset, v.byteOffset + v.byteLength) as ArrayBuffer
    }
    if (raw instanceof Blob) return await raw.arrayBuffer()
    throw new Error('场景包读回来不是字节')
}
