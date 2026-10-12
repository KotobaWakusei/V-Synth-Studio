import type {VideoInfo, VideoParse, VideoStreams} from '@/lib/api'
import {formatBytes} from '@/lib/format'

/*
 * 解析结果的读法 —— 纯函数，不碰 React 状态。
 *
 * 形状的真源是后端（`lib/api.ts` 的 `VideoParse`），这里只做界面要的换算：
 * 体积估算、编码族名、默认勾选、子目录模板、分P / 剧集摊平。
 */

/** DASH 流的码率 + 时长 → 估算体积 */
export function estimateSize(bandwidth: number | undefined, durationSec: number): string {
    const bits = Number(bandwidth)
    if (!Number.isFinite(bits) || bits <= 0 || !durationSec) return ''
    return `≈ ${formatBytes((bits / 8) * durationSec)}`
}

export function mbps(b: number | undefined): string {
    const n = Number(b)
    if (!Number.isFinite(n) || n <= 0) return '码率未知'
    return n >= 1000000 ? `${(n / 1000000).toFixed(1)} Mbps` : `${Math.round(n / 1000)} kbps`
}

/** 编码族名 —— 写在流的第二行里 */
export function codecOf(codecs: string | undefined): string {
    const c = String(codecs ?? '').toLowerCase()
    if (/hev|h265/.test(c)) return 'HEVC'
    if (/av01|av1/.test(c)) return 'AV1'
    if (/avc|h264/.test(c)) return 'H.264'
    return ''
}

export const riskyCodec = (codecs: string | undefined) => /hev|h265|av01|av1/i.test(String(codecs ?? ''))

/** 子目录模板：{title} {uploader} {date} {p} {quality}（替换与清洗规则见 `renderSubDir`） */
export function renderSubDir(parsed: VideoParse | null, qualityName: string, template: string): string {
    const t = String(template ?? '').trim()
    if (!t) return ''
    /* 没解析到结果时也要能渲染，所以这里补空对象 —— 类型写成 Partial：读的字段一律当「可能没有」处理 */
    const info: Partial<VideoInfo> = parsed?.info ?? {}
    const map: Record<string, string> = {
        title: info.title ?? '',
        uploader: info.uploader ?? '',
        date: info.publishDate ?? info.uploadDate ?? '',
        p: parsed?.currentPage?.page != null ? String(parsed.currentPage.page) : '',
        quality: qualityName,
    }
    return t
        .replace(/\{(\w+)}/g, (m, k: string) => (k in map ? String(map[k]) : m))
        .replace(/[<>:"|?*\u0000-\u001f]/g, '_')
        .replace(/[/\\]+/g, '\\')
        .replace(/\\+/g, '\\')
        .replace(/^\\|\\$/g, '')
        .trim()
}

/** 解析出**几个可选的视频**（分 P / 剧集）。yt-dlp 那边后端不发 `pages`，一个链接就是一个。 */
export function countItems(d: VideoParse | null): number {
    if (!d) return 0
    if (d.kind === 'bangumi') return (d.info?.episodes ?? []).length
    return Math.max(1, (d.info?.pages ?? []).length)
}

/** 默认选：最高画质、同画质优先 AVC/H.264（老编辑器打不开 HEVC）；音频默认 192K */
export function pickDefaults(d: VideoParse): Picked {
    if (d.source === 'ytdlp') {
        const list = (d.info?.formats ?? []).filter((f) => f.isVideo)
        return {quality: null, audio: null, formatId: list[0]?.formatId ?? null}
    }
    const videos = d.streams?.video ?? []
    const avc = videos.filter((v) => /avc|h264/i.test(v.codecs ?? ''))
    const audios = d.streams?.audio ?? []
    return {
        quality: (avc[0] ?? videos[0])?.id ?? null,
        audio: (audios.find((a) => a.id === 30280) ?? audios[0])?.id ?? null,
        formatId: null,
    }
}

/**
 * `acceptQuality` / `acceptDescription` 是平行数组：挑出「视频支持、但当前拿不到」的高画质。
 * 没有 `acceptQuality` 时退化成按名字判断。
 */
export function lockedQualities(streams: VideoStreams | null | undefined): { q: number; name: string }[] {
    const videos = streams?.video ?? []
    const qs = streams?.acceptQuality ?? []
    const ds = streams?.acceptDescription ?? []
    if (!qs.length) {
        const names = new Set(videos.map((v) => v.qualityName))
        return ds
            .filter((n) => /1080P\+|1080P60|4K|8K|HDR|杜比/.test(n) && !names.has(n))
            .map((n) => ({q: 999, name: n}))
    }
    const avail = new Set(videos.map((v) => v.id))
    return qs
        .map((q, i) => ({q, name: ds[i] ?? `画质 ${q}`}))
        .filter((x) => !avail.has(x.q) && x.q >= 80)
        .sort((a, b) => b.q - a.q)
}

/** 分P / 合集 / 剧集，摊平成同一种可选项 */
export interface Item {
    key: string
    label: string
    title: string
    durationSec?: number
    url: string
    active: boolean
}

export function itemList(parsed: VideoParse | null): { pages: Item[]; season: Item[] } {
    if (!parsed) return {pages: [], season: []}
    const info: Partial<VideoInfo> = parsed.info ?? {}
    if (parsed.kind === 'bangumi') {
        return {
            pages: [],
            season: (info.episodes ?? []).map((e, i) => ({
                key: `ep${e.epId}`,
                label: `EP${i + 1}`,
                title: e.title || e.longTitle || '',
                durationSec: e.durationSec,
                url: `https://www.bilibili.com/bangumi/play/ep${e.epId}`,
                active: e.epId === info.epId,
            })),
        }
    }
    return {
        pages: (info.pages ?? []).map((p) => ({
            key: `p${p.page}`,
            label: `P${p.page}`,
            title: p.title || '',
            durationSec: p.durationSec,
            url: `${info.url ?? ''}?p=${p.page}`,
            active: p.page === (parsed.currentPage?.page ?? 1),
        })),
        season: (info.season?.episodes ?? []).map((e, i) => ({
            key: `s${e.bvid ?? i}`,
            label: `第${i + 1}集`,
            title: e.title || '',
            durationSec: e.durationSec,
            url: `https://www.bilibili.com/video/${e.bvid}`,
            active: e.bvid === info.bvid,
        })),
    }
}

export interface Picked {
    quality: number | null
    audio: number | null
    formatId: string | null
}
