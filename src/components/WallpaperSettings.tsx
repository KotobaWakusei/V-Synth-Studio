/**
 * 设置 → 壁纸：选背景层是什么。
 *
 * 三档，落到配置里就是 `wallpaper` 一个字符串：
 *   `""`            静态图（默认；就是 `public/img/bg/` 那两张）
 *   `"we:current"`  跟随 Wallpaper Engine 当前正在用的那张
 *   `"we:<id>"`     固定用库里某一张
 *
 * ⚠️ 存配置走 `@/lib/config` 的 `saveConfig`，**不是 `api.saveConfig`**：前者会立刻
 * 更新内存快照并通知订阅者（`WallpaperLayer` 就在那儿听着），所以在设置页点一下，
 * 背景是**当场**换的，不用重启也不用切页。
 *
 * ⚠️ 库里那几类壁纸**不是都支持**：`scene` 走 webwallgl，`video`/`gif`/图片走原生
 * 标签，`web`（HTML 壁纸）和 `application`（第三方 exe）暂时没有可播的东西 —— 会退到
 * 预览图。界面上要把这件事说出来，别让用户以为「点了没反应」。
 */

import {useState} from 'react'
import {GlassSegmentedControl} from '@ttqtt/liquid-glass-react'

import {Button} from '@/components/Button'
import {Finding, Panel, PanelHead} from '@/components/Panel'
import {SwitchRow} from '@/components/SwitchRow'
import {getConfig, saveConfig} from '@/lib/config'
import {useI18n} from '@/lib/i18n'
import {useWeScan, type WeItem} from '@/lib/wallpaper'
import {type AppState, capOk} from '@/lib/types'
import type {ToastFn} from '@/pages/types'

import './WallpaperSettings.css'

const MODES = [
    {value: 'static', label: '静态图'},
    {value: 'current', label: '跟随 WE'},
    {value: 'pick', label: '库里挑'},
]

const TYPE_LABEL: Record<string, string> = {
    scene: '场景',
    video: '视频',
    web: '网页',
    application: '应用',
    gif: '动图',
    image: '图片',
}

/** 这几类现在画不出来（会退到预览图），界面上标一下。 */
const TYPE_UNSUPPORTED = new Set(['web', 'application'])

export function Wallpaper({onToast, state}: { onToast: ToastFn; state: AppState | null }) {
    const {t} = useI18n()
    const {scan, error, busy, reload} = useWeScan()
    const [sel, setSel] = useState(() => String(getConfig()['wallpaper'] ?? ''))
    const [paused, setPaused] = useState(() => Boolean(getConfig()['wallpaperPaused']))
    /* Wallpaper Engine 只有 Windows 版 —— 别的平台上这一整块没有素材来源
       （判据在后端 `platform.rs::caps`），所以只留静态图，不给「跟随 / 库里挑」。 */
    const we = state?.caps?.wallpaper
    const weOk = capOk(we)

    const mode = sel === '' ? 'static' : sel === 'we:current' ? 'current' : 'pick'
    const items = scan?.items ?? []

    const choose = (value: string, message: string) => {
        setSel(value)
        saveConfig({wallpaper: value})
        onToast(message, 'ok')
    }

    const onMode = (m: string) => {
        if (m === 'static') return choose('', '背景：静态图')
        if (m === 'current') return choose('we:current', '背景：跟随 Wallpaper Engine 当前壁纸')
        if (!items.length) {
            onToast('库里没扫到壁纸（Wallpaper Engine 没装，或者一张都没订阅）', 'warn')
            return
        }
        if (mode === 'pick') return
        choose(`we:${items[0].id}`, `背景：${items[0].title}`)
    }

    return (
        <>
            <Panel>
                <PanelHead
                    title="背景壁纸"
                    desc="只读你自己 Steam 库里的壁纸；不下载、不改动、也不会上传任何东西"
                />
                <div className="wp-mode">
                    {weOk && (
                        <GlassSegmentedControl
                            aria-label="背景来源"
                            items={MODES}
                            value={mode}
                            onValueChange={onMode}
                        />
                    )}
                    {weOk && (
                        <Button variant="ghost" icon="refresh" onClick={() => void reload()} disabled={busy}>
                            {busy ? '扫描中…' : '重新扫描'}
                        </Button>
                    )}
                </div>

                {!weOk && (
                    <Finding level="warn" title={t("这个平台用不了 Wallpaper Engine 壁纸")}>
                        {/* 原因来自后端（`platform.rs::caps` 的 `why`），键就是它的中文原文 */}
                        {t(we?.why ?? 'Wallpaper Engine 只有 Windows 版')}。
                        {t("背景只能用自带的静态图；玻璃材质、主题、配色都不受影响。")}
                    </Finding>
                )}

                {weOk && error && <p className="wp-warn">扫描失败：{error}</p>}
                {weOk && scan && !scan.found && (
                    <p className="wp-warn">
                        没找到 Wallpaper Engine（默认安装位置和 Steam 的 libraryfolders.vdf 里都没有）。
                        装在别处的话，到这里手动改一次 <code>config.json</code> 的 <code>weDir</code> 即可。
                    </p>
                )}
                {weOk && scan?.found && (
                    <p className="wp-dim">
                        Wallpaper Engine：{scan.weDir}
                        {scan.current ? ` ／ 当前：${scan.current.title}` : ''}
                    </p>
                )}
            </Panel>

            {weOk && mode === 'pick' && (
                <Panel>
                    <PanelHead title="挑一张" desc={`扫到 ${items.length} 张`}/>
                    <ul className="wp-list">
                        {items.map((it) => (
                            <li key={`${it.source}:${it.id}`}>
                                <button
                                    type="button"
                                    className="wp-item"
                                    aria-current={sel === `we:${it.id}` ? 'true' : undefined}
                                    onClick={() => choose(`we:${it.id}`, `背景：${it.title}`)}
                                >
                                    <span className="wp-title">{it.title}</span>
                                    <span className="wp-meta">
                                        <span className="wp-tag">{TYPE_LABEL[it.type] ?? it.type ?? '未知'}</span>
                                        <span className="wp-dim">{sourceLabel(it)}</span>
                                        {TYPE_UNSUPPORTED.has(it.type) && (
                                            <span className="wp-warn-inline">暂不支持，先显示预览图</span>
                                        )}
                                    </span>
                                </button>
                            </li>
                        ))}
                    </ul>
                    <p className="wp-dim">
                        视频壁纸能不能放，取决于 WebView2 解不解得了它的编码（H.264 稳，HEVC 要看系统装没装解码器）。
                    </p>
                </Panel>
            )}

            <Panel>
                <PanelHead title="性能" desc="背景层一直在动，玻璃面板每帧都要重算 —— 卡就把它停掉"/>
                <SwitchRow
                    label="暂停壁纸动画"
                    desc="停帧，不做任何渲染；界面上的静态图与玻璃不受影响"
                    checked={paused}
                    onChange={(v) => {
                        setPaused(v)
                        saveConfig({wallpaperPaused: v})
                    }}
                />
            </Panel>
        </>
    )
}

function sourceLabel(it: WeItem): string {
    if (it.source === 'workshop') return `工坊 ${it.id}`
    if (it.source === 'myprojects') return '我的项目'
    if (it.source === 'defaultprojects') return '官方内置'
    return '本地'
}
