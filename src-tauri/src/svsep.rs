//! 离线音轨分离 —— 内嵌的 Python 分离引擎
//!
//! 「炽小阳音轨分离站离线版」的 Python 后端原样内嵌：界面由本工作站的 React
//! 页面负责，Python 那边**只当 JSON API 用**（它的 `templates/` 与 `static/`
//! 不在库里，见 `tools/stage_svsep.mjs`）。
//!
//! ## 磁盘布局
//!
//! 一切都挂在**扩展包根**（`config.json` 的 `extDir`）下面；没配置时那个根就是
//! 可写目录（绿色版 `<root>/data`、安装版 `%APPDATA%\…`），也就是老行为。
//!
//! ```text
//! <扩展包根>/svsep/
//!   ├─ runtime/python.exe         Python 3.10 embeddable（下 runtime.zip 得来）
//!   ├─ backend/                   分离后端（app.py 等，**随程序分发**，不随 zip 下）
//!   ├─ bin/ffmpeg.exe             **随程序分发**（`stage_runtime_assets` 补过来）
//!   ├─ models/                    模型（**不随包发**，用户按需下载）
//!   │   ├─ UVR-MDX-NET-Inst_HQ_3.onnx 约 64 MB
//!   │   └─ BS-Roformer-SW.ckpt        约 667 MB
//!   └─ {uploads,outputs,logs,data}/   运行期数据
//! ```
//!
//! ⚠️ **`backend/` 与 `bin/` 不随下载包走**，但 `python.exe` 要与它们同级才跑得起来。
//! 所以用户一旦把扩展包换到别的盘，那两样必须**补过去**（见 [`stage_runtime_assets`]）——
//! 漏了的表现是「运行时显示已就绪，一点开始分离就报找不到 app.py」。
//!
//! ## 我们对后端源码动过的唯一一处
//!
//! `backend/config.py` 的模型目录原本只有两条路：`<BUNDLE_DIR>/models`（只读，有就赢）
//! 和 `<DATA_ROOT>/models`。我们要的第三种组合（运行时只读、模型在可写的别处）
//! 它表达不了，所以那里有一段 `CHIXIAOYANG_MODELS_DIR` 优先分支 ——
//! 由这里的 `spawn()` 传进去。改动点带注释标了「V-Synth-Studio 加的」。
//!
//! ⚠️ 别用软链接/junction 去绕这件事：那条路要处理提权、要处理「目标不存在时
//! 建不出来」，而一个环境变量就够了。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// 分离后端的默认端口。
///
/// 不用 0（随机）是因为要写进日志与界面上的「服务地址」，固定一个更好排查；
/// 它只在 127.0.0.1 上监听，且 17879 与工作站自己的 17878 错开。
const DEFAULT_PORT: u16 = 17879;

/// 端口占用时最多往后试几个
const PORT_TRIES: u16 = 20;

/// 两个模型的**完整**大小（界面用来解释「下 463 MB、占 690 MB」）。
///
/// ⚠️ 「下全了没有」的**下限**判据不在这里，在 `artifact::ARTIFACTS` 的
/// `svsep.models` 那里（`min_bytes`）—— 判据只有那一份，改判据只改那里。
/// 这里只留展示用的那个数。
const UVR_MODEL_FULL: u64 = 60 * 1024 * 1024;
const ROFORMER_MODEL_FULL: u64 = 640 * 1024 * 1024;

/// 两个模型的文件名（与上游 `config.DEFAULT_*_MODEL` 一致）。
///
/// ⚠️ 这两个名字**同时也是** `artifact::ARTIFACTS` 里 `svsep.models` 的
/// `need[0]` / `need[1]`。留这里是因为 `models_status` 的 JSON 要报 `name` 字段。
const UVR_MODEL: &str = "UVR-MDX-NET-Inst_HQ_3.onnx";
const ROFORMER_MODEL: &str = "BS-Roformer-SW.ckpt";

/// 上游 `models/` 里除权重之外的索引文件，缺一个都跑不起来
const MODEL_INDEX_FILES: &[&str] = &[
    "download_checks.json",
    "mdx_model_data.json",
    "vr_model_data.json",
    "BS-Roformer-SW.yaml",
];

/// 运行时目录的期望体积（未压缩）。
///
/// 只用于界面上「运行时 / 7.3 GB」这种展示与下载进度百分比 —— 判断装没装
/// 靠的是 `runtime_ready()`（`python.exe` 与 `backend/app.py` 两个文件在不在），
/// **不是**这个数。目录里的文件数（约 2.4 万）与单个文件名字都可能随上游
/// 换版本而变，只有「两个入口文件存在」是稳的。
pub const RUNTIME_BYTES: u64 = 7_855_000_000;

/// 运行时**压缩包**的大小（CDN 上那个 `runtime.zip`，实测 4.94 GB）。
///
/// 和上面那个 `RUNTIME_BYTES`（解压后）是两个数，别混：两处写反了会让界面
/// 把「要下多少」说成 7.9 GB。
pub const RUNTIME_ZIP_BYTES: u64 = 4_941_164_107;
/// 模型压缩包的实测大小（`models.zip`，解压后见 `models_status` 的 `expectedBytes`）。
pub const MODEL_ZIP_BYTES: u64 = 484_976_642;

/// 运行时下载链接（123 云盘 CDN，用户自己上传的包）。
///
/// ⚠️ 末尾那个 `#` **不要删**：那是用户给的原始链接，去掉它可能 404。
/// ⚠️ 换链接之前先想清楚：盘上那个 `.part` 旁边的 `.part.url` 记着旧链接，
/// 换了之后旧的半个包会被当成「别的包的」丢掉、从头下（见 `stored_resume`）。
pub const RUNTIME_URL: &str =
    "https://1856610041.cdn.123clouddisk.com/1856610041/V-Synth-Studio/runtime.zip#";

/// 模型下载链接（同上，123 云盘 CDN）。
///
/// 界面上「下载模型」按钮在没有链接时会明确说「还没配置下载地址」，
/// 而不是转圈然后失败。
pub const MODEL_URL: &str =
    "https://1856610041.cdn.123clouddisk.com/1856610041/V-Synth-Studio/models.zip#";

/// **只给开发机用的临时覆盖**（`VSS_SVSEP_MODEL_URL` / `VSS_SVSEP_RUNTIME_URL`）。
///
/// 为什么留这个口子：这两个链接是编译期常量，而「暂停 → 续传」「下载中点删除」
/// 这类事**必须在真下载跑着的时候**才能验。要是每次都改常量再重编，测完还得
/// 记得改回来 —— 漏一次就会把开发机地址发出去。用环境变量就在进程外解决。
/// ⚠️ 发布版**不要设这两个变量**，设了就是拿本地文件当下载源。
#[cfg(debug_assertions)]
fn url_override(key: &str, default: &'static str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => default.to_string(),
    }
}
#[cfg(not(debug_assertions))]
fn url_override(_key: &str, default: &'static str) -> String {
    default.to_string()
}

/// 这一次真的要用的模型下载地址（常量，或开发机用环境变量顶掉的那个）。
pub fn model_url() -> String {
    url_override("VSS_SVSEP_MODEL_URL", MODEL_URL)
}

/// 这一次真的要用的运行时下载地址。
pub fn runtime_url() -> String {
    url_override("VSS_SVSEP_RUNTIME_URL", RUNTIME_URL)
}

/* ══════════════════════════════════ 路径 ══════════════════════════════════ */

/// 运行时根目录：`<扩展包根>/svsep`
///
/// ⚠️ **这只是「默认/经典」落点** —— 真正要用的路径一律走 [`runtime_base`]：
/// 安装版装在 `Program Files` 下时这里不可写，4.7 GB 解压必然「建目录失败」；
/// 另外 C 盘紧张的用户会把整个扩展包目录指定到别的盘。
pub fn runtime_dir(ext: &Path) -> PathBuf {
    ext.join("svsep")
}

/// 这一次运行真正用的运行时目录（`python.exe` 与 `backend/` 都在它下面）。
///
/// **凡是拼运行时路径的地方都必须用它**，别再直接用 [`runtime_dir`]。
/// `ext` 是扩展包根（`artifact::ext_of`）—— 用户指定过就指向他选的那个盘。
pub fn runtime_base(ext: &Path) -> PathBuf {
    runtime_dir(ext)
}

/// 把随包分发的 `backend/` 与 `bin/` 补进运行时目录，返回补了几个文件。
///
/// 为什么需要它：用户把扩展包指定到别的盘之后，那个目录里**没有**这两个子目录 ——
/// 它们随程序分发（`bundle.resources` 的 `../data/svsep/{backend,bin}`）、不随 zip 下载，
/// 而 `python.exe` 要找的正是同级的 `backend/app.py`、`bin/ffmpeg.exe`。
/// 不补的话症状是「运行时显示已就绪，一点开始分离就报找不到 app.py」。
///
/// ⚠️ **只补缺的，不覆盖已有的**：目标目录里已经有一份就说明用户（或上一版）已经放好了，
/// 覆盖它等于把用户手动打的补丁抹掉。所以这里逐文件 `create_new` 语义地拷，不做镜像。
/// ⚠️ 默认落点（`<可写>/svsep`）下这两个目录本来就在，这里一个文件都不会动。
pub fn stage_runtime_assets(bundled: &Path, ext: &Path) -> u64 {
    let dest = runtime_base(ext);
    let mut copied = 0;
    for sub in ["backend", "bin"] {
        let from = bundled.join(sub);
        if !from.is_dir() {
            continue;
        }
        copied += copy_missing(&from, &dest.join(sub));
    }
    if copied > 0 {
        crate::log_line(&format!(
            "已把随包分发的 backend / bin 补进运行时目录（{copied} 个文件 → {}）",
            dest.to_string_lossy()
        ));
    }
    copied
}

/// 把 `from` 树下**目标里还没有**的文件拷进 `to`。返回拷贝的文件数。
///
/// ⚠️ 用 `symlink_metadata` 而不是 `metadata` 判类型：符号链接（macOS 上 libresvip 那棵
/// 树里有）跟着 `metadata` 会走到链接目标上去，`is_dir()` 判错就会把整个目标目录当文件拷。
fn copy_missing(from: &Path, to: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(from) else {
        return 0;
    };
    let mut n = 0;
    for ent in rd.flatten() {
        let src = ent.path();
        let dst = to.join(ent.file_name());
        let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if std::fs::create_dir_all(&dst).is_ok() {
                n += copy_missing(&src, &dst);
            }
            continue;
        }
        if dst.exists() {
            continue;
        }
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::copy(&src, &dst).is_ok() {
            n += 1;
        }
    }
    n
}

/* ═══════════════════ 显卡加速包（DirectML：A 卡 / Intel 核显） ═══════════════════ */

/// DirectML 版 ONNX Runtime 的 wheel（PyPI 官方文件地址，**不是** GitHub）。
///
/// 为什么需要它：UVR-MDX 那个模型走的是纯 ONNX，而上游后端**本来就写了 DirectML
/// 分支**（`separator_engine.py::_try_enable_onnx_dml`，`config.py` 也会判出 dml 模式）
/// —— 缺的只是运行时里没装 `onnxruntime-directml`。CUDA 那条只认 NVIDIA，
/// A 卡与 Intel 核显只能靠这个后端。
///
/// ⚠️ 必须是 **cp310**：随包运行时的 Python 是 3.10（见 `python310._pth`）。
/// 1.24 起 PyPI 那个包要求 Python ≥ 3.11，装上去 import 直接失败。
///
/// 实测（本机 RX 580，30 秒素材）：二轨 36 秒 → 15 秒。
pub const DML_URL: &str = "https://files.pythonhosted.org/packages/5b/f8/c9282f935b978764bdf13869cccc174267c936efd150ca070e56e23f5d05/onnxruntime_directml-1.23.0-cp310-cp310-win_amd64.whl";
/// 实测大小（`onnxruntime_directml-1.23.0-cp310-cp310-win_amd64.whl`）。
pub const DML_BYTES: u64 = 25_113_303;

/// DirectML 包的落点：`<运行时>/dml` —— 一个**额外的 site 目录**，靠 `._pth` 排在最前。
///
/// 为什么不直接覆盖 `site-packages/onnxruntime`：那会把 N 卡那份 CUDA 版**永久换掉**
/// （用户哪天插上一张 N 卡也回不去）。分两份、由 `._pth` 决定谁在前，才是可逆的。
pub fn dml_site_dir(ext: &Path) -> PathBuf {
    runtime_base(ext).join("dml")
}

/// 加速包装没装（看那个 dll 在不在）。
pub fn dml_installed(ext: &Path) -> bool {
    dml_site_dir(ext)
        .join("onnxruntime")
        .join("capi")
        .join(crate::tools::dll("onnxruntime"))
        .is_file()
}

/// 随包 Python 的 `._pth`。**嵌入版 Python 用它固定 `sys.path`，而且会忽略
/// `PYTHONPATH`** —— 想让 DirectML 那份 ORT 生效只有改这个文件一条路
/// （实测：设了 `PYTHONPATH` 也照样 import 到 `site-packages` 里那份）。
/// 文件名带版本号，所以按前缀找，将来换 3.11 不用改代码。
fn pth_file(ext: &Path) -> Option<PathBuf> {
    let dir = runtime_base(ext).join("runtime");
    let rd = std::fs::read_dir(&dir).ok()?;
    let mut hits: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            name.starts_with("python3") && name.ends_with("._pth")
        })
        .collect();
    hits.sort();
    hits.into_iter().next()
}

/// DirectML 现在生效没有（`._pth` 里有没有我们那一行）。
pub fn dml_active(ext: &Path) -> bool {
    let Some(p) = pth_file(ext) else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return false;
    };
    let want = crate::platform::clean_path(&dml_site_dir(ext));
    text.lines()
        .any(|l| crate::platform::clean_path(Path::new(l.trim())) == want)
}

/// 开关 DirectML：改 `._pth` 里那一行（幂等，可反复调）。
///
/// 行尾跟着原文件走：这个文件是 Python 自带的，别让它因为我们的编辑换一种换行。
pub fn set_dml_active(ext: &Path, on: bool) -> Result<bool, String> {
    let Some(p) = pth_file(ext) else {
        return Err("找不到随包 Python 的 ._pth（运行时布局变了？）".into());
    };
    let text =
        std::fs::read_to_string(&p).map_err(|e| format!("读不了 {}：{e}", p.to_string_lossy()))?;
    let crlf = text.contains("\r\n");
    /* 写进 `._pth` 的路径**要剥掉 `\\?\` 前缀**（`root` 常常带着它）。
    为什么非剥不可：那个前缀在 Python 的 `sys.path` 里是另一套语义，虽然实测
    CPython 3.10 能认，但不该赌；`clean_path` 就是干这件事的。
    比较时两边都归一化，所以盘上那条路径无论带不带前缀都能被认出来。 */
    let want = crate::platform::clean_path(&dml_site_dir(ext));
    let before: Vec<String> = text
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    let mut after: Vec<String> = before
        .iter()
        .filter(|l| crate::platform::clean_path(Path::new(l.trim())) != want)
        .cloned()
        .collect();
    if on {
        after.insert(0, want);
    }
    if after == before {
        return Ok(false);
    }
    let mut joined = after.join(if crlf { "\r\n" } else { "\n" });
    joined.push_str(if crlf { "\r\n" } else { "\n" });
    std::fs::write(&p, joined).map_err(|e| format!("写不了 {}：{e}", p.to_string_lossy()))?;
    crate::log_line(&format!(
        "显卡加速（DirectML）：已{}（{}）",
        if on { "开启" } else { "关闭" },
        p.to_string_lossy()
    ));
    Ok(true)
}

/// 这台机器有没有 NVIDIA 显卡。判据只看 `nvidia-smi.exe` —— 与上游 Python 同一套。
///
/// 为什么关心它：**DirectML 那份 ORT 里没有 CUDA**。给 N 卡机器开 DirectML
/// 等于把能跑 CUDA 的那份换掉，反而更慢，所以「自动」模式下有 N 卡就不开。
pub fn nvidia_present() -> bool {
    let sys = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    Path::new(&sys)
        .join("System32")
        .join("nvidia-smi.exe")
        .is_file()
}

/// 六轨（BS-RoFormer）也走 DirectML —— **要改上游那一行硬编码**。
///
/// ⚠️ 上游注释写着「RoFormer 在 DirectML 上易 OOM，仅使用 PyTorch CUDA 或 CPU」，
/// 所以它把 `use_dml` 写死成 `False`。这个开关**默认关**；开了以后显存不够是
/// **整个任务失败**（不是自动退回 CPU），所以界面上要说清「建议显存 ≥ 8 GB」。
///
/// 改法只认那一行的**前缀**（`use_dml = `）并且只在原缩进下动手，两边都能改回来；
/// 升级运行时包之后上游要是改了写法，这里会安静地不生效 —— 日志是唯一线索。
fn set_roformer_dml(ext: &Path, on: bool) -> Result<bool, String> {
    let f = runtime_base(ext)
        .join("backend")
        .join("roformer_engine.py");
    if !f.is_file() {
        return Err(format!("找不到 {}（运行时还没下？）", f.to_string_lossy()));
    }
    let text =
        std::fs::read_to_string(&f).map_err(|e| format!("读不了 {}：{e}", f.to_string_lossy()))?;
    let crlf = text.contains("\r\n");
    let mut changed = false;
    let mut out: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let target = if on {
            "        use_dml = True  # 工作站按用户设置打开（DirectML，A 卡/核显；建议显存 ≥ 8 GB）"
        } else {
            "        use_dml = False"
        };
        if line.trim_start().starts_with("use_dml = ") && line.starts_with("        ") {
            if line != target {
                changed = true;
                out.push(target.to_string());
                continue;
            }
        }
        out.push(line.to_string());
    }
    if !changed {
        return Ok(false);
    }
    let mut joined = out.join(if crlf { "\r\n" } else { "\n" });
    joined.push_str(if crlf { "\r\n" } else { "\n" });
    std::fs::write(&f, joined).map_err(|e| format!("写不了 {}：{e}", f.to_string_lossy()))?;
    crate::log_line(&format!(
        "六轨 DirectML：已{}",
        if on { "开启" } else { "关闭" }
    ));
    Ok(true)
}

/// 把设置落到运行时上。**启动时与用户改设置时都要调**。
///
/// `mode`：`"auto"`（默认）/ `"on"` / `"off"`；`six`：六轨要不要也用 DirectML。
/// 返回 `(加速生效, 六轨补丁生效)`，给日志和状态用。
pub fn apply_dml(ext: &Path, mode: &str, six: bool) -> (bool, bool) {
    let want = match mode {
        "on" => dml_installed(ext),
        "off" => false,
        // auto：装了包、而且这台机器没有 N 卡（有 N 卡就该走 CUDA 那份）
        _ => dml_installed(ext) && !nvidia_present(),
    };
    let active = match set_dml_active(ext, want) {
        Ok(_) => dml_active(ext),
        Err(e) => {
            crate::log_line(&format!("显卡加速：{e}"));
            false
        }
    };
    // 六轨补丁只在「加速真生效」时才有意义（DirectML 没生效的话那一行也不该开）
    let six_ok = match set_roformer_dml(ext, six && active) {
        Ok(_) => six && active,
        Err(e) => {
            crate::log_line(&format!("六轨 DirectML：{e}"));
            false
        }
    };
    (active, six_ok)
}

/* ── 推理方式（自动 / GPU / CPU）：加速包该不该生效由它推出来 ─────────────── */

/// 推理方式的设置文件（上游 `inference_settings.py::_SETTINGS_PATH`）。
pub fn inference_file(data_dir: &Path) -> PathBuf {
    data_dir.join("inference_settings.json")
}

/// 读盘上的推理方式。没有文件、文件坏了、值认不出，一律 `auto` —— 与上游
/// `_load_raw()` 同一套判据（服务和界面谁先写都不打架）。
pub fn read_infer_mode(data_dir: &Path) -> String {
    std::fs::read_to_string(inference_file(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("mode").and_then(Value::as_str).map(str::to_string))
        .map(|m| normalize_infer_mode(&m))
        .unwrap_or_else(|| "auto".to_string())
}

/// 认不出的一律回 `auto`（上游的 `VALID_MODES` 也只有这三个）。
pub fn normalize_infer_mode(m: &str) -> String {
    let m = m.trim().to_ascii_lowercase();
    if ["auto", "cpu", "gpu"].contains(&m.as_str()) {
        m
    } else {
        "auto".to_string()
    }
}

/// 写盘上的推理方式。格式跟上游 `set_mode()` 一样（`{"mode": …}`、两空格缩进）。
pub fn write_infer_mode(data_dir: &Path, mode: &str) -> Result<(), String> {
    let path = inference_file(data_dir);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let body = serde_json::to_string_pretty(&json!({ "mode": mode })).unwrap_or_default();
    std::fs::write(&path, body).map_err(|e| format!("写推理设置失败：{e}"))
}

/// 推理方式 → 加速包（DirectML）该不该生效，并落到 `._pth` 上。返回是否生效。
///
/// **由推理方式推出来，不再单开开关** —— 两者本来就是一件事，分开摆只会让用户
/// 在两处做同一个决定。
///
/// - `cpu`：关掉。DirectML 那份 ORT 里没有 CUDA，开着会把 `import onnxruntime`
///   顶成 DML 版，连「纯 CPU」用的都不是原来那一份。
/// - `gpu`：N 卡关（CUDA 才是快的那个），A 卡 / 核显开。
/// - `auto`：装了包、且这台机器没有 N 卡才开。
///
/// ⚠️ 六轨补丁一律传 `false`：上游说 RoFormer 走 DirectML 容易 OOM，而显存不够时是
/// **整个任务失败**（不是退回 CPU）。这里顺手把它关回去，老用户开过的也一并复位。
pub fn apply_infer_mode(ext: &Path) -> bool {
    let want = match read_infer_mode(&runtime_base(ext)).as_str() {
        "cpu" => "off",
        "gpu" => {
            if nvidia_present() {
                "off"
            } else {
                "on"
            }
        }
        _ => "auto",
    };
    apply_dml(ext, want, false).0
}

/// 下 DirectML 加速包（24 MB）→ 解到 `<运行时>/dml` → 立刻生效。
///
/// 24 MB 没必要走那套支持续传的大包机制（`.part` + 记号 + 五轮重试都在
/// `fetch_to_file` 里，够用了）。
pub async fn download_dml(
    ext: &Path,
    ctl: &DownloadCtl,
    on_progress: impl Fn(u64, Option<u64>, Stage) + Send + Sync,
) -> Result<Value, String> {
    let dest = dml_site_dir(ext);
    std::fs::create_dir_all(&dest)
        .map_err(|e| format!("建目录失败（{}）：{e}", dest.to_string_lossy()))?;
    let zip = runtime_base(ext).join("svsep-dml.whl");

    fetch_to_file(DML_URL, &zip, Some(DML_BYTES), ctl, &|got, total, stage| {
        on_progress(got, total, stage)
    })
    .await?;

    // wheel 就是个 zip，根目录里是 `onnxruntime/` 与 `…dist-info/`，整包解到 dml/ 即可
    let report = unpack(&zip, &dest, "", &on_progress)?;
    let _ = std::fs::remove_file(&zip);
    set_dml_active(ext, true)?;
    if !dml_installed(ext) {
        return Err("包解开了，但没找到 onnxruntime/capi/onnxruntime.dll —— 包结构不对？".into());
    }
    Ok(json!({
        "ok": true,
        "dir": crate::platform::clean_path(&dest),
        "files": report.files,
        "bytes": report.bytes,
    }))
}

/// 模型目录：`<扩展包根>/svsep/models`。
///
/// ⚠️ 路径形状现在**只在 `artifact::ARTIFACTS` 里声明一次**（`svsep.models`
/// 那条的 `places`）。这个函数是给下载/解包那几条链路用的**落点**，
/// 它等于 `artifact::download_dest`；判「齐没齐」别用这个目录，
/// 用 `artifact::ready` / `artifact::missing`（那才认随包那一层）。
pub fn models_dir(ext: &Path) -> PathBuf {
    /* 三个基准（`root` / 可写 / svsep）一律传 `ext`：`svsep.models` 那条只查
       `Base::Ext`，其它基准不会被用到 —— 传什么进去都不会影响结果，而传 `ext`
       至少保证「万一将来这条 places 改了」不会指到一个莫名其妙的目录。 */
    let ctx = crate::artifact::Ctx::with_dirs(ext, ext, ext, ext);
    crate::artifact::download_dest(
        &ctx,
        crate::artifact::get("svsep.models").expect("表里必须有 svsep.models"),
    )
}

fn python_exe(ext: &Path) -> PathBuf {
    runtime_base(ext).join("runtime").join("python.exe")
}

/// 运行时是否齐备（Python + 后端）。
///
/// ⚠️ 「齐的判据」**只在 `artifact::ARTIFACTS` 里声明一次**（`svsep.runtime`
/// 那条的 `need`）。这里委托过去，是为了让判据只有一份真相 ——
/// 判据改了只改那一处。
pub fn runtime_ready(ext: &Path) -> bool {
    let a = crate::artifact::get("svsep.runtime").expect("表里必须有 svsep.runtime");
    crate::artifact::ready_in(&runtime_base(ext), a)
}

/// 运行时状态（给 `/api/svsep/status` 用的那一段）。
///
/// 为什么不去统计目录里的实际字节数：2.4 万个文件、每次轮询都走一遍，
/// 在机械盘上要几秒 —— 而界面只需要「在不在」和一个够用的分母。
pub fn runtime_status(ext: &Path) -> Value {
    let dir = runtime_base(ext);
    let py = python_exe(ext);
    let backend = dir.join("backend").join("app.py");
    json!({
        "dir": dir.to_string_lossy(),
        "ready": runtime_ready(ext),
        // ⚠️ 用 `runtime_url()` 不用常量：开发机用环境变量顶掉链接时，界面显示的
        //    也得是那个顶掉的地址，否则会出现「界面说没配、其实配了」这种鬼状态。
        "downloadUrl": runtime_url(),
        // 「大概多大」用于展示与进度百分比，不是判据
        "expectedBytes": RUNTIME_BYTES as f64,
        /* 要下多少（压缩包）。界面上的「安装扩展包（约 X）」用的是它 ——
           拿 `expectedBytes` 当分母会把 4.6 GB 说成 7.9 GB。 */
        "zipBytes": RUNTIME_ZIP_BYTES as f64,
        "python": py.is_file(),
        "backend": backend.is_file(),
        "pythonPath": py.to_string_lossy(),
        "backendPath": backend.to_string_lossy(),
    })
}

/* ══════════════════════════════════ 模型 ══════════════════════════════════ */

/// 文件大小，读不到算 0。
///
/// ⚠️ 实现只有一份，在 `crate::download` —— 它是续传逻辑的核心（「从盘上重新读
/// 已有多少字节」），而续传逻辑现在也归那边管。这里只转发。
fn file_size(p: &Path) -> u64 {
    crate::download::file_size(p)
}

/// 两个模型都在不在 —— 界面上的「还没下模型」就靠这个。
///
/// ⚠️ 判据（哪两个文件、各自的字节下限）只在 `artifact::ARTIFACTS` 的
/// `svsep.models` 那条里声明一次。模型只有「扩展包根」这一层（它 <b>不</b>随包分发）。
pub fn models_ok(ext: &Path) -> bool {
    crate::artifact::ready_in_dir(&models_dir(ext), "svsep.models")
}

/// 模型状态（给 `/api/svsep/status` 用的那一段）
pub fn models_status(ext: &Path) -> Value {
    let dir = models_dir(ext);
    /* 逐项状态走 `artifact` 那一份判据（哪两个文件、各自的字节下限只在那里声明一次）。
    这里报的 `expectedSize` 用的是 `*_MODEL_FULL`（展示用的完整大小），
    与判据那个下限**是两个数**，别混。 */
    let a = crate::artifact::get("svsep.models").expect("表里必须有 svsep.models");
    /* ⚠️ 按**下标**取前两项：表里 `svsep.models.need` 的顺序就是
    [UVR 权重, Roformer 权重, 4 个索引文件]，判断两轨状态只需要前两个。
    按 `label` 去 `contains` 取太脆（改一个字就 panic），按 index 至少
    会在表被改动时于这一行立刻炸出来。 */
    let one = |i: usize| -> (String, u64) {
        let (st, n) = crate::artifact::file_state(&dir, &a.need[i]);
        (crate::artifact::state_str(st).to_string(), n)
    };
    let (uvr_state, uvr_size) = one(0);
    let (rof_state, rof_size) = one(1);
    let missing_index: Vec<&str> = MODEL_INDEX_FILES
        .iter()
        .filter(|f| !dir.join(f).is_file())
        .copied()
        .collect();
    #[allow(clippy::cast_possible_truncation)]
    let total = (uvr_size + rof_size) as f64;
    json!({
        "dir": dir.to_string_lossy(),
        // 两个都好、且索引文件齐全，才算真的就绪
        "ok": uvr_state == "ok" && rof_state == "ok" && missing_index.is_empty(),
        "downloadedBytes": total,
        "expectedBytes": (UVR_MODEL_FULL + ROFORMER_MODEL_FULL) as f64,
        // 要下多少（压缩包）；理由同 `runtime_status` 里那条
        "zipBytes": MODEL_ZIP_BYTES as f64,
        "downloadUrl": model_url(),
        "items": [
            { "key": "uvr", "name": UVR_MODEL, "label": "二轨 · 人声 / 伴奏",
              "state": uvr_state, "size": uvr_size, "expectedSize": UVR_MODEL_FULL },
            { "key": "roformer", "name": ROFORMER_MODEL, "label": "六轨 · BS-Roformer",
              "state": rof_state, "size": rof_size, "expectedSize": ROFORMER_MODEL_FULL },
        ],
        "missingIndex": missing_index,
    })
}

/* ══════════════════════ 半个包（暂停留下的续传点）══════════════════════ */

/// 某个包没下完时 `.part` 落在哪。
///
/// 两个包的落点不一样（模型的 `dest` 是 `models/`，运行时的是 `svsep/` 本身），
/// 所以别自己拼 `dest.join(...)` —— 走 `Bundle`，落点只有一个定义处。
pub fn part_path(ext: &Path, kind: &str) -> Option<PathBuf> {
    let b = match kind {
        "models" => Bundle::models(ext, None),
        "runtime" => Bundle::runtime(ext, None),
        _ => return None,
    };
    Some(b.dest.join(format!("{}.part", b.zip_name)))
}

/* `.part.url` 记号（「这半个包是谁的」）的实现只有一份，在 `crate::download`
   —— 与 bili 的下载共用同一套续传判据。下面三个是给本模块内按本地名字取的转发。

   ⚠️ 为什么这三个不能各留一份：`stored_resume` 那条「记号缺失算可以续、写着
   别的链接必须当真」的规则，是「换了服务器还接着下、拼出坏 zip」唯一的防线。
   两处各写一遍，改一处忘一处就等于没防。 */
fn url_marker(part: &Path) -> PathBuf {
    crate::download::url_marker(part)
}

fn stored_resume(part: &Path, url: &str) -> bool {
    crate::download::stored_resume(part, url)
}

/// 这个包有没有「可以接着下」的半个包，有就回它的字节数。
///
/// ⚠️ **看盘，不看内存里的记号**：工作站在下载中途被关掉、或者进程重启之后，
/// 那个 `(种类, 链接)` 的记忆就没了，而 4.7 GB 的半个包还在盘上 —— 只看内存
/// 会让界面以为「没下过」，用户一点就从零开始，白下几个 GB。
pub fn resume_point(ext: &Path, kind: &str, url: &str) -> Option<u64> {
    let part = part_path(ext, kind)?;
    if !stored_resume(&part, url) {
        return None;
    }
    let n = file_size(&part);
    (n > 0).then_some(n)
}

/// 收场之后收拾记号：暂停留着（下次还要用），下完/出错/停止都删掉。
pub fn clear_resume_marker(ext: &Path, kind: &str) {
    if let Some(part) = part_path(ext, kind) {
        let _ = std::fs::remove_file(url_marker(&part));
    }
}

/* ══════════════════════════════ 子进程管理 ══════════════════════════════ */

/// 分离服务。**由 `AppState` 持有**，进程活到工作站退出为止。
///
/// ⚠️ **只记 `writable`**：运行时 / 模型 / 数据目录全都从**扩展包根**推出来，
/// 而那个根是「进程级的当前设置」（`artifact::ext_of`）—— 用户在界面上换了目录
/// 就该立刻指到新地方，所以这里不缓存它。
pub struct Svsep {
    writable: PathBuf,
    child: Mutex<Option<Child>>,
    port: Mutex<Option<u16>>,
    /// 串行化启动流程。start() 中间会 await 健康检查；普通 Mutex 不能跨 await，
    /// 所以用 tokio mutex 防止两个调用同时各自拉起一个 Python 后端。
    start_lock: tokio::sync::Mutex<()>,
    /// 最近一次健康探测的结果与时间 —— 界面每几秒轮询一次，
    /// 没必要每次都真去打 HTTP。
    health: Mutex<Option<(Instant, bool)>>,
    /// 最近一次启动失败的原因（给界面看）
    last_error: Mutex<Option<String>>,
}

impl Svsep {
    pub fn new(writable: PathBuf) -> Self {
        Self {
            writable,
            child: Mutex::new(None),
            port: Mutex::new(None),
            start_lock: tokio::sync::Mutex::new(()),
            health: Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    pub fn writable(&self) -> &Path {
        &self.writable
    }

    /// 扩展包根（用户可能把它指到了别的盘）。
    fn ext(&self) -> PathBuf {
        crate::artifact::ext_of(&self.writable)
    }

    pub fn runtime_ready(&self) -> bool {
        runtime_ready(&self.ext())
    }

    pub fn dir(&self) -> PathBuf {
        runtime_base(&self.ext())
    }

    pub fn models(&self) -> PathBuf {
        models_dir(&self.ext())
    }

    pub fn models_ok(&self) -> bool {
        models_ok(&self.ext())
    }

    /// 运行时自己的数据目录（uploads / outputs / logs）—— 见 `runtime_base`。
    pub fn data(&self) -> PathBuf {
        runtime_base(&self.ext())
    }

    fn port(&self) -> Option<u16> {
        *self.port.lock().ok()?
    }

    pub fn base_url(&self) -> Option<String> {
        self.port().map(|p| format!("http://127.0.0.1:{p}"))
    }

    /// 端口（可能只是「上次起的那个」，不一定还在听）—— 只给界面显示用
    pub fn port_hint(&self) -> Option<u16> {
        self.port()
    }

    /// 最近一次启动失败的原因
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|e| e.clone())
    }

    /// 起服务。已经在跑就什么都不做。
    ///
    /// 返回 `(端口, 是否新起)`。
    pub async fn start(&self) -> Result<(u16, bool), String> {
        // Hold this across the probe/wait/spawn sequence: otherwise two concurrent
        // invocations can both observe "not running" and launch separate backends.
        let _start_guard = self.start_lock.lock().await;
        if let Some(p) = self.port() {
            if self.probe().await {
                return Ok((p, false));
            }
            // 进程还在、但服务不应答：先收掉再重来，否则端口会一直被占着
            self.stop();
        }
        if !self.runtime_ready() {
            return Err(format!(
                "分离运行时不在：{}\n请到「音轨分离」页下载运行时。",
                self.dir().to_string_lossy()
            ));
        }
        if !self.models_ok() {
            return Err("模型还没下载。先点上面的「下载模型」，下完再启动分离服务。".to_string());
        }

        let data = self.data();
        for sub in ["uploads", "outputs", "logs", "data"] {
            std::fs::create_dir_all(data.join(sub))
                .map_err(|e| format!("建目录失败（{}）：{e}", data.join(sub).to_string_lossy()))?;
        }
        // 模型目录也要建出来 —— 垫片要往里做联接
        let models = self.models();
        std::fs::create_dir_all(&models).map_err(|e| format!("建模型目录失败：{e}"))?;

        let exe = python_exe(&self.ext());
        let script = self.dir().join("backend").join("app.py");

        // 端口：从默认值往后试，谁先空着用谁
        let mut last: Option<String> = None;
        for offset in 0..PORT_TRIES {
            let port = DEFAULT_PORT + offset;
            if port_occupied(port) {
                continue;
            }
            match self.spawn(&exe, &script, &data, &models, port) {
                Ok(()) => {
                    // 等它把 Flask 起起来。torch / onnxruntime 第一次 import 要几秒。
                    if self.wait_ready(90).await {
                        if let Ok(mut e) = self.last_error.lock() {
                            *e = None;
                        }
                        return Ok((port, true));
                    }
                    let why = self
                        .last_error
                        .lock()
                        .ok()
                        .and_then(|e| e.clone())
                        .unwrap_or_else(|| "服务起来了但一直没应答".to_string());
                    self.stop();
                    last = Some(why);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| "起不来：默认端口往后 20 个都被占着".to_string()))
    }

    fn spawn(
        &self,
        exe: &Path,
        script: &Path,
        data: &Path,
        models: &Path,
        port: u16,
    ) -> Result<(), String> {
        let log_dir = data.join("logs");
        std::fs::create_dir_all(&log_dir).ok();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join("launch.log"))
            .ok();
        let log_err = log.as_ref().and_then(|f| f.try_clone().ok());

        // 不弹黑框：python.exe 是**控制台程序**，不显式关掉控制台窗口，用户桌面上
        // 就会顶出一个黑窗口（它的输入输出其实都被我们用管道接走了，那窗口纯属碍事）。
        // 标志和 `ipc::tools::quiet_command` 里用的是同一个：0x0800_0000 = CREATE_NO_WINDOW。
        let mut cmd = Command::new(exe);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.arg(script)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            // 上游：CHIXIAOYANG_DATA_DIR 决定 uploads/outputs/logs/data/models，
            // CHIXIAOYANG_BUNDLE_DIR 是可以有 models 的只读根
            .env("CHIXIAOYANG_DATA_DIR", data)
            .env("CHIXIAOYANG_BUNDLE_DIR", data)
            .env("CHIXIAOYANG_MODELS_DIR", models)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            /* 工作目录 = 运行时根（`python.exe` 与 `backend/` 那一层）。
            ⚠️ 它**不是**程序目录：用户把扩展包换到别的盘之后，程序目录里既没有
            `python.exe` 也没有 `backend/`，从那儿起进程会让 Python 的相对导入找不到东西。 */
            .current_dir(self.dir())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // 取消代理：后端自己也会清（config.py 里那段），但进程环境干净点更好排查
        for k in [
            "http_proxy",
            "https_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "all_proxy",
        ] {
            cmd.env_remove(k);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动分离进程失败（{}）：{e}", exe.to_string_lossy()))?;

        // 挂进作业对象：工作站一死（包括被强杀）它就跟着死。
        // 见 `job` 模块 —— `Drop` 只覆盖正常退出这条路。
        if !job::join(&child) {
            // `note!` 是 lib.rs 里的私有宏，模块里够不着，直接用底层那个函数
            crate::log_line("提示：没能把分离进程挂进作业对象，退出时只靠 Drop 收它");
        }

        // 把子进程的输出收进日志。
        //
        // ⚠️ 必须真读走：管道的缓冲区（约 64 KB）满了以后子进程会**阻塞在写日志上**，
        // 表现是任务卡在 0% 或者干脆不动 —— 这个坑很难从现象反推回原因。
        // 后端自己也往 <data>/logs/app.log 写一份，这里收的是「起不来时」才看得到的早期输出。
        if let Some(out) = child.stdout.take() {
            pump(out, log);
        }
        if let Some(err) = child.stderr.take() {
            pump(err, log_err);
        }

        if let Ok(mut c) = self.child.lock() {
            *c = Some(child);
        }
        if let Ok(mut p) = self.port.lock() {
            *p = Some(port);
        }
        if let Ok(mut h) = self.health.lock() {
            *h = None;
        }
        Ok(())
    }

    /// 停掉服务（幂等）。工作站退出时也调它。
    pub fn stop(&self) {
        if let Ok(mut p) = self.port.lock() {
            *p = None;
        }
        if let Ok(mut h) = self.health.lock() {
            *h = None;
        }
        let Ok(mut guard) = self.child.lock() else {
            return;
        };
        let Some(mut child) = guard.take() else {
            return;
        };
        kill_tree(&mut child);
    }

    /// 健康探测（带 2 秒缓存）
    pub async fn probe(&self) -> bool {
        if let Ok(h) = self.health.lock() {
            if let Some((at, ok)) = *h {
                if at.elapsed() < Duration::from_secs(2) {
                    return ok;
                }
            }
        }
        let Some(base) = self.base_url() else {
            return false;
        };
        let ok = matches!(get_json(&format!("{base}/api/status")).await, Ok(v) if is_ready(&v));
        if let Ok(mut h) = self.health.lock() {
            *h = Some((Instant::now(), ok));
        }
        ok
    }

    /// 等到服务应答为止。返回是否等到了。
    async fn wait_ready(&self, secs: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            // 进程可能已经死了（缺 dll、Python 路径不对……），别死等
            let alive = {
                let mut g = match self.child.lock() {
                    Ok(g) => g,
                    Err(_) => return false,
                };
                match g.as_mut() {
                    Some(c) => match c.try_wait() {
                        Ok(Some(status)) => {
                            let mut e = self.last_error.lock().ok();
                            if let Some(slot) = e.as_mut() {
                                **slot = Some(format!(
                                    "分离进程刚起来就退出了（退出码 {:?}）。\
                                     看一下 {} 里的日志。",
                                    status.code(),
                                    self.data()
                                        .join("logs")
                                        .join("launch.log")
                                        .to_string_lossy()
                                ));
                            }
                            false
                        }
                        Ok(None) => true,
                        Err(_) => true,
                    },
                    None => false,
                }
            };
            if !alive {
                return false;
            }
            if self.probe().await {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    }

    /* ── 转发给后端 ───────────────────────────────────────────────── */

    /// 原样转发一个 GET，回解析好的 JSON
    pub async fn get(&self, path: &str) -> Result<Value, String> {
        let base = self
            .base_url()
            .ok_or_else(|| "分离服务还没启动".to_string())?;
        get_json(&format!("{base}{path}")).await
    }

    /// 转发一个 JSON POST
    pub async fn post_json(&self, path: &str, body: &Value) -> Result<Value, String> {
        let base = self
            .base_url()
            .ok_or_else(|| "分离服务还没启动".to_string())?;
        post_json(&format!("{base}{path}"), body).await
    }

    /// 提交一次分离：把 `body` 原样 POST 给上游的 `/api/separate/...`。
    ///
    /// ⚠️ **`content_type` 和 `body` 必须是一套**：上游 Flask 要的是
    /// `multipart/form-data`（字段名 `file`，见 `ipc::svsep::multipart_body`）。
    /// 裸字节过去它读不到 `request.files["file"]`，回的是
    /// `400 {"ok":false,"error":"未检测到上传文件"}` —— 2026-10 测试版就是这么挂的。
    /// 这一层只负责发，不解析、不重拼（重拼只会在文件名转义、大 body 缓冲上出错）。
    pub async fn submit(
        &self,
        engine: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<Value, String> {
        let base = self
            .base_url()
            .ok_or_else(|| "分离服务还没启动".to_string())?;
        let path = match engine {
            "uvr" => "/api/separate/uvr",
            _ => "/api/separate/roformer",
        };
        post_raw(&format!("{base}{path}"), content_type.to_string(), body).await
    }

    /// 输出目录（整个 outputs 根，给「打开输出目录」用）。
    ///
    /// 结果位置是 `<outputs>/<task_id>/<文件名>`，**前端自己拼**（见 `api.ts::svsepFileUrl`）
    /// —— 取结果走读盘、不经过分离服务：分离完服务会自动关掉，而结果必须在那之后
    /// 还能听、还能下。上游 Python 自己也是从这个目录发的（`app.py::api_download`）。
    pub fn outputs(&self) -> PathBuf {
        self.data().join("outputs")
    }
}

impl Drop for Svsep {
    fn drop(&mut self) {
        // 工作站退出时把分离进程一起带走。
        // 它是 python.exe 而不是我们的子线程 —— 不主动收就变成孤儿进程，
        // 用户下次打开会看到两个「分离服务」在抢同一个端口。
        //
        // ⚠️ 但 Drop **收不住强杀**：任务管理器结束进程、或者运行时被跳过析构时，
        //    这个析构函数不会跑 —— python.exe 会活下来继续占着 17879，
        //    还揣着几个 GB 内存。兜底是 `join_job()`，见那里。
        self.stop();
    }
}

/* ═════════════════════════ Windows 作业对象（收子进程的兜底）════════════════════════ */

/// 子进程必须随工作站一起死 —— 连「被强杀」也算。
///
/// `Drop` 只覆盖正常退出。任务管理器强杀时析构不跑，python.exe 会变成孤儿：
/// 它继续监听 17879（下次启动时工作站会以为端口被占，另挑一个），并且占着几 GB
/// 内存不放。用户看到的是「明明关掉了，风扇还在转」。
///
/// 作业对象的解法：把子进程放进一个设了
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的作业里。**进程一死，它持有的句柄
/// 就被系统关掉**（句柄表是内核对象，正常退出、崩溃、强杀都一样会被回收），
/// 于是作业上挂着的所有进程一起被终止。这比任何用户态析构都可靠。
///
/// ⚠️ 这个句柄**故意不关**：它的生命周期就该等于本进程的。拿一个 `OnceLock`
///    存着（内核句柄，值本身是 `Send + Sync` 的），`Drop` 里关掉反而会把
///    作业提前收走。
#[cfg(windows)]
mod job {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// 句柄是内核对象，跨线程传本身是安全的 —— 用个 newtype 让编译器同意。
    struct Handle(*mut core::ffi::c_void);
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    static JOB: OnceLock<Option<Handle>> = OnceLock::new();

    /// 拿到（必要时建一个）作业对象句柄。建不出来就回 `None`，
    /// 调用方照常跑 —— 这只是兜底，失败了不该让分离功能不可用。
    fn job_handle() -> Option<*mut core::ffi::c_void> {
        JOB.get_or_init(|| unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                // 设不上限制的作业对象毫无用处：这种子进程照样会变孤儿，
                // 不如不挂（挂了反而让人以为已经兜住了）。
                return None;
            }
            Some(Handle(h))
        })
        .as_ref()
        .map(|h| h.0)
    }

    /// 把刚 spawn 出来的子进程挂进作业。挂不上就放它自己跑（有 `Drop` 兜着）。
    pub fn join(child: &std::process::Child) -> bool {
        use std::os::windows::io::AsRawHandle;
        let Some(job) = job_handle() else {
            return false;
        };
        let proc = child.as_raw_handle() as *mut core::ffi::c_void;
        unsafe { AssignProcessToJobObject(job, proc) != 0 }
    }
}

#[cfg(not(windows))]
mod job {
    /// 非 Windows 上靠进程组与 `Drop`（`kill_tree` 里有各自的实现）。
    pub fn join(_child: &std::process::Child) -> bool {
        false
    }
}

/// 把子进程的一路输出抽到日志文件里（在独立线程里读，不阻塞任何人）
fn pump<R: std::io::Read + Send + 'static>(mut r: R, mut log: Option<std::fs::File>) {
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        let reader = BufReader::new(&mut r);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if let Some(f) = log.as_mut() {
                let _ = writeln!(f, "{line}");
                let _ = f.flush();
            }
        }
    });
}

/// 收掉整棵进程树。
///
/// 只管 python.exe 不够：它会再拉起 ffmpeg 之类的孙进程。Windows 上用 taskkill /T，
/// 其它平台按进程组发信号（`quiet_command` 里已经设了 `setsid` 之类，见那边注释）。
fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id();
        let _ = crate::ipc::tools::quiet_command("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        // taskkill 已经带走了它；下面的 kill/wait 只是兜底（比如 taskkill 不存在）
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// 端口是不是被占了
fn port_occupied(port: u16) -> bool {
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_err()
}

/* ══════════════════════════════ HTTP 小工具 ══════════════════════════════ */

fn is_ready(v: &Value) -> bool {
    v.get("ok").and_then(Value::as_bool).unwrap_or(false)
        && v.get("ready").and_then(Value::as_bool).unwrap_or(false)
}

async fn get_json(url: &str) -> Result<Value, String> {
    let res = crate::net::client()
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("连不上分离服务：{e}"))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "分离服务回 HTTP {}：{}",
            status.as_u16(),
            brief(&text)
        ));
    }
    serde_json::from_str(&text).map_err(|e| format!("分离服务回的不是 JSON：{e}"))
}

async fn post_json(url: &str, body: &Value) -> Result<Value, String> {
    let res = crate::net::client()
        .post(url)
        .json(body)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| format!("请求分离服务失败：{e}"))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "分离服务回 HTTP {}：{}",
            status.as_u16(),
            brief(&text)
        ));
    }
    serde_json::from_str(&text).map_err(|e| format!("分离服务回的不是 JSON：{e}"))
}

async fn post_raw(url: &str, content_type: String, body: Vec<u8>) -> Result<Value, String> {
    let res = crate::net::client()
        .post(url)
        .header("content-type", content_type)
        .body(body)
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .map_err(|e| format!("上传到分离服务失败：{e}"))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "分离服务回 HTTP {}：{}",
            status.as_u16(),
            brief(&text)
        ));
    }
    serde_json::from_str(&text).map_err(|e| format!("分离服务回的不是 JSON：{e}"))
}

/// 出错时给用户看一小段响应体就够了，别把整页 HTML 塞进 toast
fn brief(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() <= 200 {
        return t.to_string();
    }
    let head: String = t.chars().take(200).collect();
    format!("{head}…")
}

/* ══════════════════════════════ 下载 ══════════════════════════════ */

/// 一次下载的三种收场。
#[derive(Debug)]
pub enum FetchOutcome {
    /// 下完、解好、临时文件已清
    Done(Value),
    /// 用户按了暂停：`.part` 留着，下次带 Range 接着下
    Paused { bytes: u64 },
    /// 用户按了停止：`.part` 已删，下次从头下
    Cancelled,
}

/// 下载/解压过程中的「暂停 / 停止」开关与「这次能不能续传」。
///
/// 用 `AtomicBool` 而不是 `CancellationToken`：这两个标志是**全局单例**的
/// （同一时刻只可能有一个大包在动，见 `ipc::svsep` 的 `DL_*`），
/// 引一层 token 只是为了给它找个主人。`static` 的生命周期也不受 `tokio::spawn`
/// 的 `'static` 限制。
pub struct DownloadCtl {
    pause: &'static AtomicBool,
    cancel: &'static AtomicBool,
    /// 续传时要用的链接（`None` = 这次不许续传，`.part` 视为无效）
    pub resume_url: Option<String>,
}

impl DownloadCtl {
    pub fn new(
        pause: &'static AtomicBool,
        cancel: &'static AtomicBool,
        resume_url: Option<String>,
    ) -> Self {
        pause.store(false, Ordering::Relaxed);
        cancel.store(false, Ordering::Relaxed);
        Self {
            pause,
            cancel,
            resume_url: resume_url.map(|u| u.trim().to_string()),
        }
    }

    /// 该停一下了吗（暂停或停止都算）。
    pub fn check(&self) -> bool {
        self.pause.load(Ordering::Relaxed) || self.cancel.load(Ordering::Relaxed)
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// 收场是「暂停」还是「停止」。
    ///
    /// ⚠️ 现在**没有生产代码用它**：收场的三种情形是从返回值
    /// （`FetchOutcome` / 回包里的 `paused` / `cancelled` 标志）读的，
    /// 比回头问旗标可靠。留着是因为它和 `cancelled()` 是一对语义
    /// （两个都立着时按「停止」算），单测也拿它当判据。
    #[allow(dead_code)]
    pub fn paused(&self) -> bool {
        self.pause.load(Ordering::Relaxed) && !self.cancel.load(Ordering::Relaxed)
    }
}

/// 一个可下载的包：链接、落点、zip 里那一层壳的名字、出错时怎么称呼它。
struct Bundle<'a> {
    /// svsep.rs 里那个常量（`MODEL_URL` / `RUNTIME_URL`）
    const_name: &'static str,
    /// 界面上的名字，用在错误文案里
    label: &'static str,
    /// 编译期那条链接（现在都是空串，等用户上传后填）
    default_url: &'static str,
    /// 调用方临时覆盖的链接（设置页填的、或者测试传的）
    url: Option<&'a str>,
    /// 临时 zip 放哪、解到哪
    dest: PathBuf,
    /// 从条目名里剥掉的一层（见 `extract_zip`）
    strip: &'static str,
    /// 解压完删掉 zip 时，它叫什么
    zip_name: &'static str,
}

impl<'a> Bundle<'a> {
    fn models(ext: &Path, url: Option<&'a str>) -> Self {
        Self {
            const_name: "MODEL_URL",
            label: "模型",
            default_url: MODEL_URL,
            url,
            dest: models_dir(ext),
            strip: "models/",
            zip_name: "svsep-models.zip",
        }
    }

    fn runtime(ext: &Path, url: Option<&'a str>) -> Self {
        Self {
            const_name: "RUNTIME_URL",
            label: "运行时",
            default_url: RUNTIME_URL,
            url,
            dest: runtime_base(ext),
            strip: "", // 留着 runtime/ 这一层：那边要的正是 runtime/python.exe
            zip_name: "svsep-runtime.zip",
        }
    }

    /// 实际用哪条链接：调用方给的优先，空串等于没给
    fn resolved_url(&self) -> &str {
        match self.url {
            Some(u) if !u.trim().is_empty() => u.trim(),
            _ => self.default_url.trim(),
        }
    }
}

/// 进度回调报的是**哪一段**：还在下，还是已经在解压。
///
/// 这只是 [`crate::download::Stage`] 的别名 —— 保留下划线是为了不动 svsep 内部
/// 那几十处 `Stage::Download` / `Stage::Extract`，以及 `midi_transcribe` 那边
/// 已有的 `crate::svsep::Stage` 引用。
pub use crate::download::Stage;

/// 把 `DownloadCtl` 翻成引擎要的 [`crate::download::Control`]。
///
/// ⚠️ 引擎收的是两个**谓词**而不是两个旗标，因为 bili 那边根本没有「暂停」
/// 这个概念 —— 合成一个必然让一边撒谎。svsep 这边两个旗标都有，如实翻译即可。
fn control(ctl: &DownloadCtl) -> crate::download::Control<'_> {
    // 「该停了吗」= 暂停或停止；「算取消吗」= 只有停止
    crate::download::Control::new(|| ctl.check(), || ctl.cancelled())
}

/// 盘上那个 `.part` 是不是**一整个 zip**（而不是下了一半就断的）。
///
/// 什么时候会是「一整包」：字节都下完了，收尾（解压）没跑完 —— 解压途中
/// 关窗口、解压报错、磁盘满。`.part` 与旁边的链接记号都还在，于是「点继续下载」
/// 会带着 `Range: bytes=<整包长>-` 再发一次请求：服务端回 416 就永远下不动，
/// 回 200（不认 Range 的服务器）就把几个 GB **从头下一遍**。
/// 包既然已经完整，一个字节都不该再要 —— 直接去解压（见 `fetch_bundle` 开头）。
///
/// 判据 = 中央目录读得出来，而且**每个条目的数据段都真的落在文件里**
/// （`data_offset + 30 + compressed_size <= 文件大小`；本地头里的 name/extra
/// 长度 ≥ 0，所以这是个放宽版判据）。半截包连中央目录都读不出来 —— 目录在文件
/// 末尾，压根没下到 —— 第一步就返回 false 了。
///
/// ⚠️ 成本 = 读一遍中央目录（runtime 那个约 2.4 MB），所以只在**点下载时**调，
///    别放进每 2 秒一次的状态轮询里。
fn part_is_whole_zip(zip_path: &Path) -> bool {
    let Ok(f) = std::fs::File::open(zip_path) else {
        return false;
    };
    /* ⚠️ 判据就是「中央目录读得出来」，**不加**「每个条目的数据段都落在文件里」
       的逐个检查：中央目录在文件**末尾**，而「下载没下完」的第一个症状就是
       EOCD 被削掉 —— 两种判据在整包的**每一个截断点**上结论都一致，那条检查
       想挡的东西已经被这一步挡住了，而它要为两万多个条目各做一次比较。

       ⚠️ 但别把它当成「更强的检查」：它比的是**文件长度**，不是「内容有没有洞」。
       那半个包若是被乱序写到文件里的（`.part` 断点续传就是这么写的），长度够、
       EOCD 也在，两个判据都会说「是一整包」。真正该防的那种坏包由解压时的
       CRC 校验兜住（见 `extract_zip`）。 */
    let Ok(z) = zip::ZipArchive::new(f) else {
        return false;
    };
    !z.is_empty()
}

/// 下载一个包、解压、清掉临时文件。`download_models` / `download_runtime` 都走这里。
///
/// 三种收场，见 `FetchOutcome`：下完解好 / 用户按了暂停（留着 `.part`，下次接着下）
/// / 用户按了停止（删掉 `.part`，下次从头下）。
///
/// ⚠️ **中途断了会自己接着下**：链接被掐断时歇 10 秒再发一次请求，最多 5 轮
///    （`Retry::BIG_PACK`），每轮都带 `Range` 从**盘上已有的字节**接着下（不是从头下）。
///    5 轮都不成才报错，那半个包留着 —— 界面上还能点「继续下载」接着下。
///
/// ⚠️ **续传是按 `.part` 在不在判的**：文件在那儿就发 `Range: bytes=<已有>-`，
///    服务端回 206 就从那儿接着写。回到 200（不认 Range 的服务器，比如某些
///    简单的静态托管）就**从头写**：追加会得到一个前一段 + 整段拼起来的坏 zip，
///    而且要到解压时才炸 —— 那比重新下更糟。
/// ⚠️ 下载途中写的是 `<dest>/<zip_name>.part`，**不是 `.zip`** —— 万一用户
///    中途去点了「开始分离」，`runtime_ready()` 看到的是半个 zip，不会把它
///    当成装好了。
/// ⚠️ **开工前先看盘上那个 `.part` 是不是一整包**（下载完了、解压没收尾）：
///    是就一个字节都不再要，直接解压（见 `part_is_whole_zip`）。
async fn fetch_bundle(
    b: &Bundle<'_>,
    ctl: &DownloadCtl,
    on_progress: &(impl Fn(u64, Option<u64>, Stage) + Send + Sync),
) -> Result<FetchOutcome, String> {
    let url = b.resolved_url();
    if url.is_empty() {
        return Err(format!(
            "还没配置{}下载地址。打包好的 {} 需要先传到服务器，\
             再把地址填进 svsep.rs 的 {}。",
            b.label, b.zip_name, b.const_name
        ));
    }
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(format!("{}下载地址必须是 http(s) 链接", b.label));
    }

    std::fs::create_dir_all(&b.dest).map_err(|e| format!("建目录失败：{e}"))?;
    let zip_path = b.dest.join(format!("{}.part", b.zip_name));

    // 已有多少字节（上次暂停留下的）。调用方说不能续传时当成 0，并且把旧的那个
    // 半个文件删掉 —— 留着它只会让下次误判。
    //
    // ⚠️ 光有 `resume_url` 还不够：那个链接必须和 `.part` 旁边记的**对得上**，
    //    否则这半个包是别的文件的，接上去会拼出一个坏 zip（见 `stored_resume`）。
    /* ★ 盘上那个 `.part` 要是**一整包**，一个字节都别再要 —— 直接去解压。

    什么情况会是「一整包」：字节都下完了，但收尾（解压）没跑完 —— 解压途中关窗口、
    解压报错、磁盘满。`.part` 与链接记号都还在，于是「点继续下载」会带着
    `Range: bytes=<整包长>-` 再发一次请求，把几 GB 从头再下一遍。
    ⚠️ 这个判断必须排在「不续传就把 `.part` 删掉」**之前**：那条会把一整包也删了。
    记号在不在都不影响这个判断 —— 包是完整的，记号只管续传。 */
    let whole = part_is_whole_zip(&zip_path);

    /* 真的去拉字节 —— 走 `crate::download`，与 bili 的视频下载共用同一份。这里
       只剩「包在哪儿、叫什么、续传点认不认」这三件**这个宿主特有**的事。 */
    let outcome = if whole {
        // 一整包已经躺在盘上了：不发请求、不写记号，直接去解压
        crate::download::Outcome::Done { bytes: file_size(&zip_path) }
    } else {
        /* ⚠️ **调用方说了不许续传就把旧 `.part` 删掉。**
        这是 `DownloadCtl::resume_url` 的全部含义：调用方查过盘（`resume_for`），
        只有「上次就是暂停在这个包、同一条链接上」才会给 `Some(url)`。给 `None`
        说明这半个包要么属于别的链接、要么上次是出错/停止结束的 —— 留着它接新
        链接的 `Range` 会拼出坏包（要到解压才炸）。

        ⚠️ 删了之后仍然把 `resume` 置 `true`（下面那行）：**轮内重试**必须能从
        盘上接着下。这两件事不同 —— `resume_url` 管「认不认开工前那个旧的」，
        `resume` 管「断了之后连不连着下」。合并成一个就会让「调用方没指定链接时
        重试也从头下」，白烧用户几个 GB。 */
        if ctl.resume_url.is_none() {
            let _ = std::fs::remove_file(&zip_path);
            let _ = std::fs::remove_file(url_marker(&zip_path));
        }
        let base = crate::download::Request {
            url,
            label: b.label,
            part: &zip_path,
            headers: &[],
            // 单流：这几 GB 的包要的是「断了能接着下」，不是并行
            // 几 GB 的包：断了接着下（`Resumable` 自带「不 probe」，见 Shape）
            shape: crate::download::Shape::Resumable,
            client: crate::download::default_client(),
            retry: crate::download::Retry::BIG_PACK,
            expect: None,
        };
        let note = |got: u64, total: Option<u64>, stage: Stage| on_progress(got, total, stage);
        match crate::download::download(&base, &control(ctl), &note).await {
            Ok(o) => o,
            Err(e) => {
                /* ⚠️ 万一这一轮其实已经把整包下到手、只是收尾报了错：就地改判成
                   「下完了」去解压。不改判的话，下次点「继续下载」会带着「整包长」
                   的 Range 再发一次请求 —— 那是把几个 GB 从头下一遍。 */
                if part_is_whole_zip(&zip_path) {
                    crate::download::Outcome::Done { bytes: file_size(&zip_path) }
                } else {
                    /* ⚠️ 解压失败/下载失败时**不删** `.part`，记号也不删：删掉 =
                       用户得把几 GB 再下一遍（流量是他自己的）。留着的话界面会显示
                       「已经下好 N，接着下」，用户再点一次走的是 `part_is_whole_zip`
                       那条短路。 */
                    return Err(format!(
                        "{e}；已下的 {} 字节留在盘上，下次点「继续下载」从这儿接着下",
                        file_size(&zip_path)
                    ));
                }
            }
        }
    };
    /* 两种收场直接返回；`Done` 不看字节数 —— 下完就是下完，接下来要拿的是
       `extract_zip` 报出来的条目数（真正的成果），不是下了多少字节。 */
    match outcome {
        crate::download::Outcome::Done { .. } => {}
        crate::download::Outcome::Paused { bytes } => {
            return Ok(FetchOutcome::Paused { bytes });
        }
        crate::download::Outcome::Cancelled => return Ok(FetchOutcome::Cancelled),
    }

    /* 下面这一段是**解压**、不是下载。`unpack` 负责把阶段切成 `Extract`
       （界面据此把标签从「正在下载…」改成「正在解压…」），别在调用点各写一遍
       那两行样板，见 `unpack` 的注释。 */
    let report = unpack(&zip_path, &b.dest, b.strip, on_progress);
    match report {
        Ok(report) => {
            // 解好了才删：它有几 GB，留着没用（`.part` 这名字也保证下次不会被当成
            // .zip 用）。记号跟着走 —— 没有「半个包」可续了。
            let _ = std::fs::remove_file(&zip_path);
            let _ = std::fs::remove_file(url_marker(&zip_path));
            Ok(FetchOutcome::Done(json!({
                "ok": true,
                "dir": b.dest.to_string_lossy(),
                "files": report.files,
                "bytes": report.bytes,
            })))
        }
        Err(e) => {
            /* ⚠️ 解压失败时**不删** `.part`，记号也不删：删掉 = 用户得把几 GB 再
            下一遍（流量是他自己的）。留着的话界面会显示「已经下好 N，接着下」，
            用户再点一次走的是 `part_is_whole_zip` 那条短路 —— 不发请求，直接重解。 */
            Err(e)
        }
    }
}

/// 下载模型 zip 并解压到 `<扩展包根>/svsep/models/`。
///
/// 用户可以暂停 / 停止（`ctl`），暂停后 `.part` 留着、下次 `resume_url` 指同一条
/// 链接就接着下；停止会把 `.part` 删掉，下次从头下。
pub async fn download_models(
    ext: &Path,
    url: &str,
    ctl: &DownloadCtl,
    on_progress: impl Fn(u64, Option<u64>, Stage) + Send + Sync + 'static,
) -> Result<FetchOutcome, String> {
    let given = if url.trim().is_empty() {
        None
    } else {
        Some(url)
    };
    let b = Bundle::models(ext, given);
    // 三种收场统一成「一个带 paused / cancelled 标志的对象」，界面只看这两个
    // 标志决定进度条是消失还是留着。落盘状态（`models` / `runtime`）由
    // `ipc::svsep` 拼 —— 它同时也在拼 `/api/svsep/status`。
    Ok(match fetch_bundle(&b, ctl, &on_progress).await? {
        FetchOutcome::Done(mut v) => {
            if let Some(o) = v.as_object_mut() {
                o.insert("paused".into(), Value::Bool(false));
                o.insert("cancelled".into(), Value::Bool(false));
            }
            FetchOutcome::Done(v)
        }
        FetchOutcome::Paused { bytes } => FetchOutcome::Done(json!({
            "ok": true, "paused": true, "cancelled": false, "bytes": bytes,
        })),
        FetchOutcome::Cancelled => FetchOutcome::Done(json!({
            "ok": true, "paused": false, "cancelled": true, "bytes": 0,
        })),
    })
}

/// 下载运行时 zip 并解压到 `<扩展包根>/svsep/`（`runtime/` 那一层留着）。
///
/// ⚠️ 这个包**几 GB**，只该下一次：文件多、解压慢。换工作站版本时运行时通常
/// 不变 —— 变的是 `backend/` 那几个 .py，而**那部分随程序打包**，不走这里。
pub async fn download_runtime(
    ext: &Path,
    url: &str,
    ctl: &DownloadCtl,
    on_progress: impl Fn(u64, Option<u64>, Stage) + Send + Sync + 'static,
) -> Result<FetchOutcome, String> {
    let given = if url.trim().is_empty() {
        None
    } else {
        Some(url)
    };
    let b = Bundle::runtime(ext, given);
    Ok(match fetch_bundle(&b, ctl, &on_progress).await? {
        FetchOutcome::Done(mut v) => {
            if let Some(o) = v.as_object_mut() {
                o.insert("paused".into(), Value::Bool(false));
                o.insert("cancelled".into(), Value::Bool(false));
            }
            FetchOutcome::Done(v)
        }
        FetchOutcome::Paused { bytes } => FetchOutcome::Done(json!({
            "ok": true, "paused": true, "cancelled": false, "bytes": bytes,
        })),
        FetchOutcome::Cancelled => FetchOutcome::Done(json!({
            "ok": true, "paused": false, "cancelled": true, "bytes": 0,
        })),
    })
}

/// 把一条链接下成一个**指定路径**的文件，回来时文件是完整的。
///
/// 给「不需要续传的小包」用 —— 人声转 MIDI 的模型包（364 MB）和 ONNX Runtime
/// （那个 zip 78 MB，解出来只要里面 15 MB 的 dll）都走这里。和
/// `download_models` / `download_runtime` 的区别：
///
///   * **不要 `Bundle`**：那两个的落点、剥层、`.part` 位置都写死在一张表里，
///     这两个包不在这张表内，硬塞进去只会让那张表长出两个特例分支。
///   * **不做续传**：重试是**从 0 开始**（`got` 每轮归零）。364 MB 断一次重下
///     能接受，而续传要维护 `.part.url` 记号、`stored_resume` 比对、以及
///     「半个包其实是一整包」那条短路 —— 为一个包付这些复杂度不划算。
///     ⚠️ 要加续传就得把上面那三样一起加齐，只加一半会拼出坏 zip。
///   * **失败会把半截文件删掉**：留下它没有任何东西会去认领（没有记号、没有
/// 下载一个文件到 `out`（**最终文件名**，不是 `.part`），返回字节数。
///
/// ## 与 `fetch_bundle` 的区别
///
/// 这个函数服务的是「几十~几百 MB、一次下完、失败了从头重来也不心疼」的包
/// （DirectML 加速包 24 MB、MIDI 模型 364 MB、ONNX Runtime 78 MB）。
/// 那个是几 GB、必须支持续传的。
///
/// | | `fetch_to_file` | `fetch_bundle` |
/// |---|---|---|
/// | `Shape` | `Plain`（每轮从 0 开始） | `Resumable` |
/// | 落点 | `out` 就是成品名 | `<dest>/<zip>.part`，调用方再解压 |
/// | 大小核对 | `expect`（有就核对） | 无（解压时才知道对不对） |
///
/// ⚠️ **重试只归引擎**（`Retry::BIG_PACK`，5 轮 × 10 秒）：在这儿再套一层循环
/// 会叠成 25 轮。大小核对（`expect`）由引擎算作「这一轮失败」，这里不另判。
pub async fn fetch_to_file(
    url: &str,
    out: &Path,
    expect: Option<u64>,
    ctl: &DownloadCtl,
    on_progress: impl Fn(u64, Option<u64>, Stage) + Send + Sync,
) -> Result<u64, String> {
    let req = crate::download::Request {
        url,
        label: "文件",
        /* ⚠️ `out` 是**最终文件名**（不是 `.part`）。`Plain` 会保证失败之后
           盘上不留半截文件 —— 留下的话没有东西会去认领它，只会让人以为下过了。 */
        part: out,
        headers: &[],
        shape: crate::download::Shape::Plain,
        client: crate::download::default_client(),
        /* 5 轮 × 10 秒，和 `fetch_bundle` 一致：网络抖动通常一两轮就过去了，
           真接不上（DNS 挂了、被墙）第五轮也还是接不上。
           ⚠️ 这里用的就是**引擎自己的**重试循环 —— 大小核对（`expect`）在引擎里
           也算「这一轮失败」，于是「截断的响应 → 等 10 秒 → 删掉重下」这条路
           不用在这里另写一遍。 */
        retry: crate::download::Retry::BIG_PACK,
        // 「官方大小」：对不上就当这一轮失败（GitHub release 偶发截断响应）
        expect,
    };
    let note = |got: u64, total: Option<u64>, stage: Stage| on_progress(got, total, stage);
    match crate::download::download(&req, &control(ctl), &note).await {
        Ok(crate::download::Outcome::Done { bytes }) => Ok(bytes),
        /* 暂停 —— 这个路径没有续传，所以和停止一样：引擎已经把半截文件删了。
           返回一句能看懂的话而不是假装成功（调用方会把它当失败报给界面）。 */
        Ok(crate::download::Outcome::Paused { .. }) => {
            Err("下载已暂停。再点一次会从头下（这个包不支持续传）。".into())
        }
        Ok(crate::download::Outcome::Cancelled) => Err("已停止".into()),
        Err(e) => Err(e),
    }
}

/// 删掉一个目录里所有 `*.part`（没下完的半个 zip）与它旁边的 `*.part.url`，
/// 返回删掉的文件数与字节数。
///
/// 为什么单独来一遍：`.part` 的落点是 `Bundle::dest`，模型的在 `models/` 里
/// （会被上面的递归带走），**但运行时的在 `svsep/` 那一层**，不在
/// `runtime/` 里 —— 不专门扫一遍就会留下几 GB 的半个 zip，而界面显示「已删除」。
/// ⚠️ `.part.url` 只有几十字节，但**必须一起删**：留着它而 `.part` 没了，
/// 下次 `resume_point` 会看到「记号在、包不在」，白查一遍（虽然也不会出错）。
fn sweep_part_files(dir: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0u64, 0u64);
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (files, bytes);
    };
    for ent in rd.flatten() {
        let p = ent.path();
        let is_part = p
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(".part") || n.ends_with(".part.url"))
            .unwrap_or(false);
        if !is_part || !p.is_file() {
            continue;
        }
        let size = ent.metadata().map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(&p).is_ok() {
            files += 1;
            bytes += size;
        }
    }
    (files, bytes)
}

/// 一键删除所有「下下来的依赖」：模型、运行时、ffmpeg。
///
/// ⚠️ **运行时也在里面**，用户点之前必须知道：删完要重新下 4.7 GB 才能用分离。
/// ⚠️ **只删「下下来的」那几层，不是整个 `svsep/`**。这个区别是最容易写错的地方：
///    `<扩展包根>/svsep/` 下面还住着 `backend/`（分离后端的 .py，**随程序打包、
///    不该删**）和运行期的 `data/ logs/ outputs/ uploads/`（用户的东西）。
///    所以要拼三个具体目录：
///      · 模型   `<扩展包根>/svsep/models`
///      · 运行时 `<扩展包根>/svsep/runtime`
///      · ffmpeg `<扩展包根>/svsep/bin`（跟运行时同一个包里的，见
///        `backend/config.py::_ensure_ffmpeg_on_path` —— 删了等于没装）
/// ⚠️ 三个目录都是一个一个删文件（不是 `remove_dir_all`）：几万个文件里总有几个
///    被别的进程占着（杀软扫描、残留的 python），一个失败就整段放弃最糟 ——
///    那会留下一个「删了一半、界面还说有 7 GB」的目录。删不掉的记下来照实报。
/// ⚠️ 只删目录**里面**的东西，目录本身留着：`models/` 是 `MODEL_DIR`，
///    引擎启动时会检查它在不在。
/// `cancelled` 是「用户按了停止」的探针（删除几万个文件要几十秒）。
/// `on_progress` 收 `FnMut` —— 它的调用方基本都是就地改一个计数器，
/// 收 `Fn` 会逼着每个人套一层 `Cell`。
pub fn delete_dependencies(
    writable: &Path,
    cancelled: impl Fn() -> bool,
    mut on_progress: impl FnMut(u64, u64),
) -> Value {
    let ext = crate::artifact::ext_of(writable);
    let svsep = runtime_base(&ext);
    let targets = [
        ("模型", models_dir(&ext)),
        ("运行时", svsep.join("runtime")),
        ("ffmpeg", svsep.join("bin")),
    ];
    let mut removed_bytes: u64 = 0;
    let mut removed_files: u64 = 0;
    let mut locked: Vec<String> = Vec::new();
    let mut stopped = false;

    // ⚠️ 没下完的半个 zip 先单独扫一遍，**而且只扫 `svsep/` 这一层**（不递归）：
    //    `.part` 的落点就是 `Bundle::dest` —— 模型包的 `dest` 是 `models/`（会跟着
    //    下面的 walk 一起走），运行时包的 `dest` 是 `svsep/` 本身，它**不在**那三个
    //    目标目录里面，不专门扫就会留下几 GB 的半个 zip，而界面显示「已删除」。
    //    ⚠️ 这一遍必须在 walk **之前**、且不能放进下面那个循环里：放进循环会把
    //    `models/` 里的 `.part` 数两遍（先扫掉一次，walk 时文件已经没了但计数早加过），
    //    于是报「已删 7 个」而实际只有 6 个文件。
    //    ⚠️ 加上 `<可写>/svsep` 那一份**是为了老落点**：用户把扩展包换到别的盘之后，
    //    原来那个盘上可能还留着半个 zip（几 GB），而按钮写着「删除全部依赖」。
    //    两个目录相同时 `sweep_part_files` 第二次扫到的是空目录，不会重复计数。
    for d in [svsep.clone(), writable.join("svsep")] {
        let (f, b) = sweep_part_files(&d);
        removed_files += f;
        removed_bytes += b;
    }

    'outer: for (label, dir) in targets {
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            if cancelled() {
                stopped = true;
                break 'outer;
            }
            let rd = match std::fs::read_dir(&d) {
                Ok(rd) => rd,
                // 目录不在 = 没什么可删，不是错误
                Err(_) => continue,
            };
            for ent in rd.flatten() {
                if cancelled() {
                    stopped = true;
                    break 'outer;
                }
                let p = ent.path();
                let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir {
                    stack.push(p);
                    continue;
                }
                let size = ent.metadata().map(|m| m.len()).unwrap_or(0);
                match std::fs::remove_file(&p) {
                    Ok(()) => {
                        removed_files += 1;
                        removed_bytes += size;
                        // 界面每 200 个文件刷一次就够（它 2 秒才轮询一次状态）
                        if removed_files % 200 == 0 {
                            on_progress(removed_files, removed_bytes);
                        }
                    }
                    Err(_) => {
                        if locked.len() < 8 {
                            locked.push(format!("{label}：{}", p.display()));
                        }
                    }
                }
            }
        }
    }
    on_progress(removed_files, removed_bytes);

    json!({
        "ok": true,
        "cancelled": stopped,
        "removedBytes": removed_bytes,
        "removedFiles": removed_files,
        // 界面只报「有 N 个文件删不掉」，不把 8 条路径全铺开
        "lockedCount": locked.len(),
        "locked": locked,
    })
}

#[derive(Debug)]
pub struct ExtractReport {
    pub files: u64,
    pub bytes: u64,
}

/// 解压一个包，并把进度**切成 `Extract` 阶段**。
///
/// 下载与解压共用一条进度通道，所以这两行都得有：
///
/// ```ignore
/// on_progress(0, None, Stage::Extract);
/// let report = extract_zip(&zip, &dest, strip, |done, all| {
///     on_progress(done, Some(all), Stage::Extract)
/// })?;
/// ```
///
/// ⚠️ 漏掉第一行的表现：界面上的条子「冲到 100% → 归零 → 在同一个『正在下载…』
/// 标签下再爬一遍」，像是下完又自动重下了一次。漏掉第二行则表现为「解压那一段
/// 没有进度、条子停着不动」。
///
/// 两行都得记得，那就别让每个调用点自己记 —— 收成一处。
///
/// ⚠️ 它与 `extract_zip` 的关系是「加一层进度语义」，不是替代：要自己控制进度
/// 回调形状的地方（`extract_zip` 的单测）仍直接用它。
pub fn unpack(
    zip_path: &Path,
    dest: &Path,
    strip: &str,
    on_progress: &(impl Fn(u64, Option<u64>, Stage) + Send + Sync),
) -> Result<ExtractReport, String> {
    // 归零 + 换阶段：界面据此把标签从「正在下载…」改成「正在解压…」
    on_progress(0, None, Stage::Extract);
    extract_zip(zip_path, dest, strip, |done, all| {
        on_progress(done, Some(all), Stage::Extract)
    })
}

/// 解一个 zip 到 `dest`，返回解出来多少。
///
/// `strip` 是要从每个条目名前面剥掉的一层目录名（带斜杠），剥不掉就原样保留。
/// 两个包的布局不同，必须显式说清楚：
///   * `svsep-models.zip` 里是 `models/…`，解到 `<可写>/svsep/` 要剥掉 `models/`；
///   * `svsep-runtime.zip` 里是 `runtime/…`，解到 `<root>/data/svsep/` 要**留着**
///     （那边正好需要 `runtime/python.exe` 这一层）。
/// 解到哪一层写错不会报错，只会在用户点「开始分离」时才现形。
///
/// ## 为什么用 `zip` crate 而不是自己写
///
/// 自己写一份解析器要重新走一遍 EOCD 扫描、Zip64 换值、中央目录遍历、本地头
/// 跳转、deflate 解码、zip slip 防护 —— 而：
///
/// * **规范细节都得重新对一遍**。EOCD 从文件尾往回找几 KB、Zip64 的两个哨兵、
///   「中央目录偏移是 0xFFFFFFFF 时占位符在哪儿」……这些规范里都写清楚了，
///   而 crate 实现的正是同一份规范，还带着官方测试矢量。
/// * **不整块读进内存**。runtime.zip 里有单个条目接近 1 GB，先读整个压缩体再解
///   等于为了解一个文件先申请 1 GB。crate 是流式的（`io::copy` 边解边写）。
/// * **CRC 校验**。crate 默认逐条核对解出来的内容完整性。
/// * **zip slip 是安全边界**。防目录穿越写错一次就是任意文件写入，
///   交给 `enclosed_name()`（它按 Windows 语义切 `..`，反斜杠也算分隔符）。
///
/// ## `strip` 与 `enclosed_name` 的顺序
///
/// ⚠️ **必须先 `strip` 再交给 `enclosed_name`**，不能反过来：两个包的布局都是
/// 「顶层一个目录」，`strip` 正是用来剥掉它。顺序错了要么剥不掉、要么把
/// `enclosed_name` 已经算好的相对路径又切一刀。这两步是分开的两件事：
/// `strip` 管「解出来放哪一层」，`enclosed_name` 管「这个条目名安不安全」。
pub fn extract_zip(
    zip_path: &Path,
    dest: &Path,
    strip: &str,
    // `FnMut` 而不是 `Fn`：单测要往 Vec 里记进度，`Fn` 连这个都做不到
    // （真实调用点只是往全局静态里写，两者都满足）。
    mut on_progress: impl FnMut(u64, u64),
) -> Result<ExtractReport, String> {
    let f = std::fs::File::open(zip_path).map_err(|e| format!("打开 zip 失败：{e}"))?;
    let mut zip =
        zip::ZipArchive::new(f).map_err(|e| format!("不是有效的 zip（读不出中央目录）：{e}"))?;
    let mut report = ExtractReport { files: 0, bytes: 0 };
    let mut done_src: u64 = 0;

    /* ⚠️ 分母先算出来，别写在循环里 —— runtime 有 2.7 万个条目，每轮重新 sum
       一遍就是几千万次加法。
       ⚠️ `.max(1)` 要与下面 `done_src` 的口径**一模一样**：零字节条目（目录、
       空文件）按 1 计数，否则分子会超过分母、进度条冲到 100% 以上。
       ⚠️ 用 `compressed_size()`（盘上的字节数）而不是 `size()`（解出来的字节数）：
       进度条量的是「还剩多少要读」，而解压时间大致正比于**压缩后**的字节。 */
    let mut total_src: u64 = 0;
    for i in 0..zip.len() {
        let e = zip.by_index(i).map_err(|e| format!("读第 {i} 个条目失败：{e}"))?;
        total_src += e.compressed_size().max(1);
    }

    for i in 0..zip.len() {
        let mut e = zip.by_index(i).map_err(|e| format!("读第 {i} 个条目失败：{e}"))?;
        // 跳过的条目（目录、空名）也要推进进度，否则分母算了它们、
        // 分子没算，进度条就永远到不了头（单测钉着这一条）。
        done_src += e.compressed_size().max(1);

        let raw_name = e.name().replace('\\', "/");
        /* ⚠️ 用 `enclosed_name()` 而不是 `mangled_name()`/`sanitized_name()`：
           后两者会**静默地把 `..` 改掉**，于是危险条目变成「一个名字很怪的正常
           文件」被正常解出来 —— 那是把安全问题藏起来。我们宁可当场报错
           （用户看得懂「zip 里的路径不安全」）。 */
        let safe = e.enclosed_name().ok_or_else(|| {
            format!("zip 里的路径不安全（会解到目标目录外面）：{raw_name}")
        })?;
        let rel = safe.to_string_lossy().replace('\\', "/");
        let rel = rel.trim_start_matches("./");
        let rel = if strip.is_empty() {
            rel
        } else {
            rel.strip_prefix(strip).unwrap_or(rel)
        };

        /* 目录条目不算文件。⚠️ **必须用 `e.is_dir()`**，不能看「名字以 `/` 结尾」：
           `enclosed_name()` 是拿 `PathBuf` 拼的，`models/` 到那儿已经变成
           `models`（尾斜杠被规范化掉了）—— 照名字判的话目录会被当成文件解出来，
           `report.files` 于是比真实文件数多（单测当场抓到）。 */
        if !rel.is_empty() && !e.is_dir() {
            let out = dest.join(rel);
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
            }
            /* 流式解 —— 不把整个压缩体先读进 `Vec`。
               ⚠️ 错误里必须带上条目名：`io` 错误（磁盘满、文件被占用）本身
               指不到是哪个文件，两万多条里靠猜是查不出来的。 */
            let mut w =
                std::fs::File::create(&out).map_err(|er| format!("建「{raw_name}」失败：{er}"))?;
            std::io::copy(&mut e, &mut w)
                .map_err(|er| format!("解「{raw_name}」失败（写到一半）：{er}"))?;
            report.files += 1;
            report.bytes += w.metadata().map(|m| m.len()).unwrap_or(0);
        }
        on_progress(done_src, total_src);
    }
    Ok(report)
}

/* ══════════════════════════════════ 单测 ══════════════════════════════════ */

#[cfg(test)]
mod tests {
    use super::*;

    /// 运行时落在**扩展包根**下的 `svsep/`；扩展包根没配置时就是可写目录。
    ///
    /// 这一条钉的是「默认行为一步都没变」：老用户（`extDir` 空）的运行时与模型
    /// 仍然在原来那个地方，改动只是把「根」抽出来可配置。
    #[test]
    fn the_runtime_lives_under_the_extension_root() {
        let writable = std::env::temp_dir().join("vss-svsep-ext-root");
        // 没配置 → 根就是可写目录（`artifact::ext_of` 的默认）
        assert_eq!(crate::artifact::ext_of(&writable), writable);
        assert_eq!(runtime_base(&writable), writable.join("svsep"));
        assert_eq!(
            models_dir(&writable),
            writable.join("svsep").join("models"),
            "模型与运行时并排在 svsep/ 下"
        );
        assert_eq!(dml_site_dir(&writable), writable.join("svsep").join("dml"));
    }

    /// 换了扩展包目录之后，`svsep/` 下面必须把随包分发的 `backend/` 与 `bin/` 补上。
    ///
    /// 不补的症状：运行时显示「已就绪」，一点开始分离就报找不到 `app.py`（那两个
    /// 目录随程序分发、不在 runtime.zip 里）。顺带钉住「只补缺的、不覆盖已有的」——
    /// 覆盖会把用户在盘上手改过的那份抹掉。
    #[test]
    fn staging_copies_the_bundled_backend_and_bin() {
        let base = std::env::temp_dir().join("vss-svsep-stage");
        let _ = std::fs::remove_dir_all(&base);
        let bundled = base.join("data").join("svsep");
        let ext = base.join("ext");

        std::fs::create_dir_all(bundled.join("backend")).unwrap();
        std::fs::write(bundled.join("backend").join("app.py"), b"print(1)").unwrap();
        std::fs::create_dir_all(bundled.join("bin")).unwrap();
        std::fs::write(bundled.join("bin").join("ffmpeg.exe"), b"MZ").unwrap();

        let n = stage_runtime_assets(&bundled, &ext);
        assert_eq!(n, 2, "backend/app.py 与 bin/ffmpeg.exe 各一个");
        assert!(runtime_base(&ext).join("backend").join("app.py").is_file());
        assert!(runtime_base(&ext).join("bin").join("ffmpeg.exe").is_file());

        // 再补一次：什么都不缺，一个都不拷
        assert_eq!(stage_runtime_assets(&bundled, &ext), 0);
        // 目标里已有的**不许覆盖**（那是用户手改过的那份）
        std::fs::write(runtime_base(&ext).join("backend").join("app.py"), b"mine").unwrap();
        std::fs::write(bundled.join("backend").join("extra.py"), b"x").unwrap();
        assert_eq!(stage_runtime_assets(&bundled, &ext), 1, "只补新增的那个");
        assert_eq!(
            std::fs::read(runtime_base(&ext).join("backend").join("app.py")).unwrap(),
            b"mine"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// DirectML 的开关就是 `python310._pth` 里那一行 —— 幂等、可逆、行尾跟着原文件。
    #[test]
    fn dml_switch_flips_the_pth_line_both_ways() {
        let ext = std::env::temp_dir().join("vss-svsep-dml-test");
        let _ = std::fs::remove_dir_all(&ext);
        let rt = runtime_dir(&ext).join("runtime");
        std::fs::create_dir_all(&rt).unwrap();
        let pth = rt.join("python310._pth");
        std::fs::write(&pth, "python310.zip\n.\n\n# c\nimport site\n").unwrap();

        // 夹具里没有 dml 包 → `installed` 是 false（判据是那个 dll）
        assert!(!dml_installed(&ext));

        // 开：那一行插到**最前面**（必须排在 `import site` 之前才抢得到 onnxruntime）
        assert!(set_dml_active(&ext, true).unwrap());
        let text = std::fs::read_to_string(&pth).unwrap();
        let first = text.lines().next().unwrap();
        assert_eq!(first, dml_site_dir(&ext).to_string_lossy());
        assert!(text.contains("import site"), "原来的内容不能弄丢：{text:?}");
        assert!(dml_active(&ext));

        // 再开一次什么都不做（幂等）
        assert!(!set_dml_active(&ext, true).unwrap());

        // 关：那一行没了，其余照旧
        assert!(set_dml_active(&ext, false).unwrap());
        let text = std::fs::read_to_string(&pth).unwrap();
        assert!(!dml_active(&ext));
        assert!(text.starts_with("python310.zip"));
        assert!(text.contains("import site"));

        // 行尾跟着原文件：本来是 LF 就还是 LF
        assert!(!text.contains("\r\n"));
        let _ = std::fs::remove_dir_all(&ext);
    }

    /// 六轨补丁只认那一行的前缀，两边都能改回来（上游升级了也不会被改坏）。
    #[test]
    fn roformer_patch_is_reversible_and_ignores_foreign_code() {
        let ext = std::env::temp_dir().join("vss-svsep-roformer-test");
        let _ = std::fs::remove_dir_all(&ext);
        let backend = runtime_dir(&ext).join("backend");
        std::fs::create_dir_all(&backend).unwrap();
        let f = backend.join("roformer_engine.py");
        std::fs::write(
            &f,
            "use_ac = bool(plan.get(\"use_autocast\"))\n        # RoFormer 易 OOM\n        use_dml = False\n",
        )
        .unwrap();

        assert!(set_roformer_dml(&ext, true).unwrap());
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(t.contains("use_dml = True"), "{t}");
        assert!(
            t.contains("use_ac = bool(plan.get(\"use_autocast\"))"),
            "别的行不许动"
        );
        // 幂等
        assert!(!set_roformer_dml(&ext, true).unwrap());
        // 关得回来
        assert!(set_roformer_dml(&ext, false).unwrap());
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(t.contains("        use_dml = False"), "{t}");
        let _ = std::fs::remove_dir_all(&ext);
    }

    /* ⚠️ 三态判据（缺 / 半个 / 齐）的测试在
    `artifact::tests::file_state_separates_partial_from_missing` —— 实现只有
    `artifact` 那一份，测试跟着实现走；那条还多覆盖了「0 字节算 Missing」与
    「min_bytes=0 时存在即算齐」。 */

    #[test]
    fn brief_cuts_long_bodies() {
        assert_eq!(brief("  hi  "), "hi");
        let long = "x".repeat(500);
        let b = brief(&long);
        assert!(b.ends_with('…'));
        assert_eq!(b.chars().count(), 201);
    }

    #[test]
    fn ready_needs_both_ok_and_ready() {
        assert!(is_ready(&json!({"ok": true, "ready": true})));
        assert!(!is_ready(&json!({"ok": true})));
        assert!(!is_ready(&json!({"ok": false, "ready": true})));
        assert!(!is_ready(&json!({"ok": "ready"})));
    }

    /* ══ extract_zip ══════════════════════════════════════════════════════
    两个包的**真实**内容（485 MB / 4.7 GB）不进仓库，而偏移量算错肉眼审不出来，
    所以这里现场造一个两种压缩方式都有的小 zip，把整条路走一遍。 */

    /// 造一个最小但合法的 zip。`deflate = true` 走压缩（方式 8），否则原样存（方式 0）。
    ///
    /// ⚠️ **必须用 crate 自己的写入器**，别手写字节拼装（本地头 + 中央目录 +
    /// EOCD + 自算 CRC32）：写入器与读取器共用同一套 CRC 实现，而手算的那份
    /// 一旦算错，读取器不校验 CRC 时测试照样绿。crate 的读取器默认开校验，
    /// 所以下面那些用例也顺带验了「内容没被解坏」。
    fn make_zip(items: &[(&str, &[u8], bool)]) -> Vec<u8> {
        use std::io::Write;
        let mut out: Vec<u8> = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
            for &(name, data, deflate) in items {
                let method = if deflate {
                    zip::CompressionMethod::Deflated
                } else {
                    zip::CompressionMethod::Stored
                };
                let opts = zip::write::SimpleFileOptions::default().compression_method(method);
                // 目录条目（名字以 `/` 结尾）要走 add_directory，否则 crate 会把它
                // 当普通文件写 —— 而 `is_dir()` 是按名字判的，两者结果一样，
                // 但走对的方法更贴近真包（PowerShell 也是这么造的）。
                if name.ends_with('/') {
                    w.add_directory(name, opts).unwrap();
                } else {
                    w.start_file(name, opts).unwrap();
                    w.write_all(data).unwrap();
                }
            }
            w.finish().unwrap();
        }
        out
    }

    #[test]
    fn extract_zip_handles_both_methods_and_strip() {
        let base = std::env::temp_dir().join("vss-svsep-zip-ok");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let zip_path = base.join("t.zip");
        let dest = base.join("out");

        let big = "BS-Roformer".repeat(500); // 1 KB 重复文本，deflate 会真的压
        let stored: Vec<u8> = (0u16..=511).map(|i| (i % 251) as u8).collect();
        let zip = make_zip(&[
            ("models/", b"", true), // 纯目录条目 —— 必须被跳过
            ("models/a.onnx", big.as_bytes(), true),
            ("models/b.ckpt", &stored, false),
            ("outside.txt", "strip 没命中".as_bytes(), true),
        ]);
        std::fs::write(&zip_path, &zip).unwrap();

        let mut ticks: Vec<(u64, u64)> = Vec::new();
        let rep = extract_zip(&zip_path, &dest, "models/", |d, t| ticks.push((d, t))).unwrap();

        assert_eq!(rep.files, 3, "目录条目不该算成一个文件");
        assert_eq!(
            std::fs::read_to_string(dest.join("a.onnx")).unwrap(),
            big,
            "deflate 条目解出来必须字节一致"
        );
        assert_eq!(
            std::fs::read(dest.join("b.ckpt")).unwrap(),
            stored,
            "存（方式 0）条目解出来必须字节一致"
        );
        // strip 只对以它开头的条目生效，没命中的原样保留
        assert!(dest.join("outside.txt").is_file());
        assert_eq!(dest.join("models").join("a.onnx").exists(), false);
        assert_eq!(ticks.len(), 4, "每**条目**报一次进度（含被跳过的目录）");
        let (done, total) = *ticks.last().unwrap();
        assert_eq!(done, total, "最终进度必须刚好 100%");
        assert!(ticks.windows(2).all(|w| w[0].0 <= w[1].0), "进度只能往前走");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// ★ `unpack` 必须**先报一次「阶段切到 Extract」**，再按解压进度报。
    ///
    /// 钉住的是一段容易漏的约定：不切阶段的话界面上的条子会「冲到 100% → 归零 →
    /// 在同一个『正在下载…』标签下再爬一遍」，看着像下完又自动重下了一次。
    #[test]
    fn unpack_announces_the_extract_stage_before_starting() {
        let inner = "BS-Roformer-SW".repeat(50);
        let zip = make_zip(&[("models/a.onnx", inner.as_bytes(), true)]);
        let dir = std::env::temp_dir().join("vss-unpack-stage");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.zip");
        std::fs::write(&p, &zip).unwrap();

        let ticks = Mutex::new(Vec::<(u64, Option<u64>, Stage)>::new());
        let rep = unpack(&p, &dir.join("out"), "", &|got, total, stage| {
            ticks.lock().unwrap().push((got, total, stage));
        })
        .unwrap();
        assert!(rep.files >= 1);

        let t = ticks.lock().unwrap().clone();
        assert!(!t.is_empty(), "一次进度都没报");
        assert_eq!(
            t[0],
            (0, None, Stage::Extract),
            "第一条进度必须是「归零 + 切到解压阶段」：{t:?}"
        );
        assert!(
            t[1..].iter().all(|(_, _, s)| *s == Stage::Extract),
            "解压期间不该混进下载阶段：{t:?}"
        );
        assert!(
            t[1..].iter().all(|(_, total, _)| total.is_some()),
            "解压阶段的进度要带分母（不然界面画不出条子）：{t:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_zip_keeps_the_top_level_when_strip_is_empty() {
        // runtime 包就是这么用的：必须**留住** `runtime/` 那一层，
        // 因为后面找的是 `<root>/data/svsep/runtime/python.exe`。
        let base = std::env::temp_dir().join("vss-svsep-zip-nostrip");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let zip_path = base.join("t.zip");
        let dest = base.join("out");
        std::fs::write(
            &zip_path,
            make_zip(&[("runtime/python.exe", b"MZ fake", false)]),
        )
        .unwrap();

        extract_zip(&zip_path, &dest, "", |_, _| {}).unwrap();
        assert!(dest.join("runtime").join("python.exe").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }


    /// 拿**真的**包跑一遍 —— 单测里的 zip 是我自己造的，而用户手上那两个包是
    /// PowerShell 的 `ZipFile.CreateFromDirectory` 造的（可能用数据描述符、
    /// 可能有 Zip64、字段排布也可能不一样），两个包都不进仓库，
    /// 所以这条用环境变量守着，平时直接跳过。
    ///
    ///     $env:VSS_REAL_ZIP='H:\工作站\资料归档\models.zip'; cargo test --bins reads_the_real -- --nocapture
    ///
    /// ⚠️ 这条验的是**我们对 `zip` crate 的用法**（zip64 换值、EOCD 扫描那些
    /// 归 crate）：`strip` 剥对没有、大条目解得出来没有、而且**解出来的字节数与
    /// 中央目录里写的 `size()` 一致**（后者是 crate 的 CRC 校验之外我们唯一还能
    /// 自己盯的东西）。
    #[test]
    fn reads_the_real_packaged_zip_when_asked() {
        let Ok(path) = std::env::var("VSS_REAL_ZIP") else {
            return;
        };
        let dest = std::env::temp_dir().join("vss-realzip-out");
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();

        // 中央目录里声明的最大条目（用来核对解出来的字节数）
        let (count, big_name, big_size) = {
            let f = std::fs::File::open(&path).expect("打开真包失败");
            let mut z = zip::ZipArchive::new(f).expect("真包读不出中央目录");
            let mut best: Option<(String, u64)> = None;
            for i in 0..z.len() {
                let e = z.by_index(i).unwrap();
                // `size()` 是解压后的字节数，`compressed_size()` 是盘上的
                if best.as_ref().is_none_or(|(_, s)| e.size() > *s) {
                    best = Some((e.name().to_string(), e.size()));
                }
            }
            let (n, s) = best.expect("一个条目都没读到");
            (z.len(), n, s)
        };
        println!("{path}：{count} 条，最大条目 {big_name:?} 解压后 {big_size} 字节");
        assert!(big_size > 400_000_000, "最大的条目也太小了：{big_size}");

        // 整包解一遍 —— `strip` 传空（真包的顶层目录由调用方按包决定，
        // 这里只验「能完整解开」）
        let rep = extract_zip(Path::new(&path), &dest, "", |_, _| {}).expect("真包解不开");
        println!("解出 {} 个文件 / {} 字节", rep.files, rep.bytes);
        assert!(rep.files > 0, "一个文件都没解出来");
        assert!(
            dest.join(&big_name).is_file(),
            "最大的那个条目没解出来：{big_name}"
        );
        let got = std::fs::metadata(dest.join(&big_name)).unwrap().len();
        assert_eq!(
            got, big_size,
            "解出来的字节数与中央目录里声明的不一致（写截断了？）"
        );

        let _ = std::fs::remove_dir_all(&dest);
    }

    /// 走一遍**真的下载链路**：HTTP 流式下载 → 解压 → 删临时文件 → 复查状态。
    ///
    /// 这条刻意不去解 runtime（几 GB、十几分钟），而是解最小的那个真包
    /// `models.zip`（462 MB）。下载那一层代码 `fetch_bundle` 两个包共用，
    /// 而**分层**已经分别被覆盖过：`extract_zip` 的 `strip=""` 与 Zip64 偏移
    /// 由 `extracts_the_whole_real_runtime_pack_when_asked` 管，这里只管
    /// 「字节真的从网上流下来、进度回调真的被调、`.part` 真的被删掉」。
    ///
    /// 落点必须写在 `VSS_REAL_DOWNLOAD_DEST`（会往里写 730 MB）—— 默认写
    /// `%TEMP%`。**绝不要把它指向真的可写目录**：那会把用户装好的模型重下一遍。
    ///
    ///     $env:VSS_REAL_MODELS_URL='http://127.0.0.1:18080/models.zip'
    ///     $env:VSS_REAL_DOWNLOAD_DEST='H:\工作站\tmp-svsep-dl'
    ///     cargo test --bins downloads_the_real_models_pack -- --nocapture
    #[tokio::test]
    async fn downloads_the_real_models_pack_when_asked() {
        let Ok(url) = std::env::var("VSS_REAL_MODELS_URL") else {
            return;
        };
        let dest = std::env::var("VSS_REAL_DOWNLOAD_DEST")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("vss-svsep-download-real"));
        // 每次都从**空目录**开始：这条测试要看的就是「没有 → 有」这个转变，
        // 留着上次的产物就证明不了任何事。
        let _ = std::fs::remove_dir_all(&dest);

        // 进度回调：既要能调，也要真的在涨（下载一半就断会停在很小的数上）
        let seen = std::sync::Arc::new(Mutex::new((0u64, 0usize)));
        let seen2 = seen.clone();
        let t = Instant::now();
        // 这次真跑不允许暂停/停止，只验「下完 → 解好 → 临时文件清掉」
        static NO_PAUSE: AtomicBool = AtomicBool::new(false);
        static NO_STOP: AtomicBool = AtomicBool::new(false);
        let ctl = DownloadCtl::new(&NO_PAUSE, &NO_STOP, None);
        let out = download_models(&dest, &url, &ctl, move |got, _total, _stage| {
            let mut s = seen2.lock().unwrap();
            s.0 = got;
            s.1 += 1;
        })
        .await
        .expect("下载真包失败");
        let (got, ticks) = *seen.lock().unwrap();
        println!(
            "下了 {got} 字节（{ticks} 次进度回调），耗时 {:?}；收场 {}",
            t.elapsed(),
            match out {
                FetchOutcome::Done(_) => "Done",
                FetchOutcome::Paused { .. } => "Paused",
                FetchOutcome::Cancelled => "Cancelled",
            }
        );
        assert!(got > 400_000_000, "下载字节数太少：{got}");
        assert!(ticks > 10, "进度回调只被调了 {ticks} 次");

        let models = models_dir(&dest);
        for name in [
            UVR_MODEL,
            ROFORMER_MODEL,
            "download_checks.json",
            "BS-Roformer-SW.yaml",
        ] {
            assert!(
                models.join(name).is_file(),
                "解压后缺文件：{name}（落点 {}）",
                models.display()
            );
        }
        // 最大的那个模型不能是空壳
        let big = std::fs::metadata(models.join(ROFORMER_MODEL))
            .unwrap()
            .len();
        assert!(
            big > 400_000_000,
            "{ROFORMER_MODEL} 只有 {big} 字节，没解全"
        );

        // 临时 zip 与 `.part` 都该没了 —— 它有几 GB，留着毫无用处
        for leftover in ["svsep-models.zip", "svsep-models.zip.part"] {
            assert!(
                !models.join(leftover).exists(),
                "临时文件没删掉：{leftover}"
            );
        }
        // 状态复查要走通（界面就是靠它把按钮换成「已就绪」的）
        let st = models_status(&dest);
        println!("models_status = {st}");
        assert_eq!(st["ok"], Value::Bool(true), "状态复查说模型没齐");

        let _ = std::fs::remove_dir_all(&dest);
    }

    /// 走一遍**真的暂停 → 续传**：下到一半立暂停旗标，看 `.part` 留着；再带着
    /// `resume_url` 下第二次，看它真的从断点接上（不是从头下）。
    ///
    /// 为什么非要真打一次 HTTP：`Range` 的语义是**服务端**的事 —— 我们发
    /// `bytes=N-`，服务端可以回 206，也可以不理会回 200 一整份。两种情况下代码
    /// 都得不出坏包，而这件事只有真的挂上一个会回 206 的服务器才验得出来。
    /// （本地那个 `python -m http.server` 就回 206；测试用的 18080 也是它。）
    ///
    ///     $env:VSS_REAL_MODELS_URL='http://127.0.0.1:18080/models.zip'
    ///     $env:VSS_REAL_RESUME_DEST='H:\工作站\tmp-svsep-resume'
    ///     cargo test --bins pause_and_resume -- --nocapture
    #[tokio::test]
    async fn pauses_and_resumes_the_real_models_pack_when_asked() {
        let Ok(url) = std::env::var("VSS_REAL_MODELS_URL") else {
            return;
        };
        let dest = std::env::var("VSS_REAL_RESUME_DEST")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("vss-svsep-resume-real"));
        let _ = std::fs::remove_dir_all(&dest);
        let models = models_dir(&dest);
        std::fs::create_dir_all(&models).unwrap();
        let part = models.join("svsep-models.zip.part");

        /* ── 第一轮：下到 12 MB 就暂停 ───────────────────── */
        static PAUSE: AtomicBool = AtomicBool::new(false);
        static STOP: AtomicBool = AtomicBool::new(false);
        let ctl = DownloadCtl::new(&PAUSE, &STOP, None);
        let marks = std::sync::Arc::new(Mutex::new(0u64));
        let marks2 = marks.clone();
        let out = download_models(&dest, &url, &ctl, move |got, _, _stage| {
            let mut m = marks2.lock().unwrap();
            *m = got;
            // ⚠️ 旗标是**另一条手臂**在真实场景里立的（HTTP 请求进来），这里就地立；
            //    立在回调里等价 —— `fetch_bundle` 每写完一块就查一次。
            if got > 12_000_000 {
                PAUSE.store(true, Ordering::Relaxed);
            }
        })
        .await
        .expect("第一轮下载失败");
        let paused_at = *marks.lock().unwrap();
        /* ⚠️ 别断言 `FetchOutcome::Paused` —— `download_models` / `download_runtime`
        会把三种收场**统一成 `Done(一个带标志的对象)`**（上面那段 match：
        `{"ok":true,"paused":true,"bytes":N}`），因为 HTTP 那一层只认一种形状。 */
        let flag = |v: &FetchOutcome, k: &str| -> bool {
            match v {
                FetchOutcome::Done(j) => j[k] == Value::Bool(true),
                _ => false,
            }
        };
        assert!(
            flag(&out, "paused"),
            "下到 {paused_at} 字节时立了暂停，收场却不是「暂停」：{out:?}"
        );
        assert!(!flag(&out, "cancelled"), "只是暂停，不该算成停止");
        let half = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        println!("暂停在 {paused_at} 字节处，盘上 .part = {half} 字节");
        assert!(half > 8_000_000, "暂停后 .part 太小：{half}");
        assert!(half < 484_000_000, "暂停后 .part 已经是一整包了：{half}");
        // 暂停**不能**留下解压产物 —— 半个包解不出东西来
        assert!(
            !models.join(ROFORMER_MODEL).exists(),
            "还没下完就解压出模型了"
        );

        /* ── 第二轮：带 resume_url 接着下 ────────────────── */
        let ctl2 = DownloadCtl::new(&PAUSE, &STOP, Some(url.clone()));
        assert!(
            ctl2.paused() == false && ctl2.cancelled() == false,
            "构造时该清旗标"
        );
        let resumed_from = ctl2.resume_url.clone().unwrap();
        assert_eq!(resumed_from, url);
        let seen = std::sync::Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
        let seen2 = seen.clone();
        let t = Instant::now();
        let out2 = download_models(&dest, &resumed_from, &ctl2, move |got, total, _stage| {
            seen2.lock().unwrap().push((got, total.unwrap_or(0)));
        })
        .await
        .expect("续传失败");
        let ticks = seen.lock().unwrap().len();
        let first = seen.lock().unwrap().first().copied().unwrap_or((0, 0));
        println!(
            "续传耗时 {:?}，{ticks} 次进度回调，第一次是 {first:?}",
            t.elapsed()
        );
        /* ★ 这条才是「真的续上了」的证据：`fetch_bundle` 在开始拉数据**之前**
        先回调一次 on_progress(got = already, total = 剩余 + already)。要是没带
        Range / 服务端没认，第一次回调会是 (0, 整包大小) —— 那就成了从头下。 */
        assert!(
            first.0 >= half,
            "续传第一次回调是 {first:?}，比盘上已有的 {half} 字节还少 —— 这是在从头下"
        );
        /* ⚠️ 总长是 `Content-Length: 484976642`（= 整个 `models.zip` 的字节数），
        不是 `models_status` 里那个 `expectedBytes`。 */
        assert_eq!(first.1, 484_976_642, "续传报的总长不对：{first:?}");
        assert!(flag(&out2, "paused") == false && flag(&out2, "cancelled") == false);
        assert!(
            matches!(out2, FetchOutcome::Done(_)),
            "续传没有下完：{out2:?}"
        );

        // 下完就该跟没暂停过一样：模型齐、.part 与 .zip 都清掉
        for name in [
            UVR_MODEL,
            ROFORMER_MODEL,
            "download_checks.json",
            "BS-Roformer-SW.yaml",
        ] {
            assert!(models.join(name).is_file(), "续传后缺文件：{name}");
        }
        assert!(!part.exists(), "下完了 .part 还在");
        assert!(!models.join("svsep-models.zip").exists(), "下完了 zip 还在");
        let st = models_status(&dest);
        assert_eq!(st["ok"], Value::Bool(true), "续传后状态复查说没齐");

        let _ = std::fs::remove_dir_all(&dest);
    }

    /* ══ 断线重试：接着下，不从头下 ═══════════════════════════════════════

    要验的就是一句话：「下载期间链接失败 → 自己再试，而且是接着刚刚那个
    字节继续下，不是从头下」。⛔ **这条全程在 127.0.0.1 上跑，一个字节的
    公网流量都不花** —— 真包是 462 MB / 4.7 GB，用户的流量按 GB 算钱。
    「断线 / Range 起点 / 拼出来的包完不完整」本地这个服务器全能验。 */

    /// 从一段请求头里取出 `Range: bytes=N-` 的 N（没带就回 `None`）。
    ///
    /// ⚠️ **大小写不能当准**：HTTP 头名不区分大小写，客户端发出去的是 `range:`。
    fn range_start(head: &str) -> Option<usize> {
        head.lines()
            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
            .and_then(|l| l.split('=').nth(1))
            .and_then(|v| v.trim().trim_end_matches('-').parse::<usize>().ok())
    }

    /// 一个只会说 HTTP/1.1 的最小服务器：**第一次**请求声明整个长度、却只写半份
    /// 就关掉连接（= 链接中途断了），之后认 `Range` 回 206 + 剩下的部分。
    ///
    /// 收到的请求头都记进返回的 `Vec` 里 —— 测试拿它当「有没有带 Range 接着下」
    /// 的证据。每条响应都带 `Connection: close`，免得客户端复用那条被掐掉的连接。
    fn half_then_resume_server(payload: Vec<u8>) -> (String, std::sync::Arc<Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        std::thread::spawn(move || {
            let total = payload.len();
            let mut n = 0usize;
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { continue };
                n += 1;
                // 读请求头就够（GET，没有请求体）
                let mut buf = [0u8; 4096];
                let read = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..read]).to_string();
                log2.lock().unwrap().push(head.clone());
                let from = range_start(&head).unwrap_or(0).min(total);
                let (code, declared, body): (&str, usize, &[u8]) = if n == 1 {
                    // ⚠️ 长度声明成**整个**、只发一半：客户端才认得出「少了半截」，
                    //    这才是真实世界里「下到一半链接断了」的样子
                    ("200 OK", total, &payload[..total / 2])
                } else {
                    ("206 Partial Content", total - from, &payload[from..])
                };
                let mut resp = format!(
                    "HTTP/1.1 {code}\r\nContent-Length: {declared}\r\nConnection: close\r\n"
                );
                if code.starts_with("206") {
                    resp.push_str(&format!(
                        "Content-Range: bytes {from}-{}/{total}\r\n",
                        total.saturating_sub(1)
                    ));
                }
                resp.push_str("\r\n");
                let _ = s.write_all(resp.as_bytes());
                let _ = s.write_all(body);
                let _ = s.flush();
                // 关连接：第一条就这样收场，客户端读到的是「少了半截」
            }
        });
        (format!("http://127.0.0.1:{port}/x.zip"), log)
    }

    #[tokio::test]
    async fn a_broken_download_retries_and_resumes_from_the_bytes_on_disk() {
        // 造一个**真的 zip**：「拼出来的包完不完整」才有硬判据 ——
        // 断点接错位置（比如又从头写）解压就直接失败。
        let inner = "BS-Roformer-SW".repeat(400);
        let zip = make_zip(&[
            ("models/a.onnx", inner.as_bytes(), true),
            ("models/b.ckpt", &[7u8; 3000], false),
        ]);
        let (url, reqs) = half_then_resume_server(zip.clone());

        let dest = std::env::temp_dir().join("vss-svsep-retry-test");
        let _ = std::fs::remove_dir_all(&dest);
        static PAUSE: AtomicBool = AtomicBool::new(false);
        static STOP: AtomicBool = AtomicBool::new(false);
        let ctl = DownloadCtl::new(&PAUSE, &STOP, None);
        let ticks = std::sync::Arc::new(Mutex::new(Vec::<(u64, Option<u64>, Stage)>::new()));
        let ticks2 = ticks.clone();

        let b = Bundle::models(&dest, Some(&url));
        let t = Instant::now();
        let out = fetch_bundle(&b, &ctl, &move |got, total, stage| {
            ticks2.lock().unwrap().push((got, total, stage));
        })
        .await
        .expect("断线之后应该自己接着下，结果整条失败了");
        println!(
            "断线 → 重试 → 下完，耗时 {:?}（含一轮 10 秒等待）",
            t.elapsed()
        );
        assert!(matches!(out, FetchOutcome::Done(_)), "没下完：{out:?}");

        // 解出来的字节必须和原包**一模一样** —— 接错位置这里就会炸
        let models = models_dir(&dest);
        assert_eq!(
            std::fs::read(models.join("a.onnx")).unwrap(),
            inner.as_bytes(),
            "接着下拼出来的包不对（断点接错位置了）"
        );
        assert_eq!(std::fs::read(models.join("b.ckpt")).unwrap().len(), 3000);
        // 下完就该收拾干净：`.part` 与记号都不留
        let part = models.join("svsep-models.zip.part");
        assert!(!part.exists(), "下完了 .part 还在");
        assert!(!url_marker(&part).exists(), "下完了 .part.url 还在");

        /* ★ 这两条才是「接着下」的证据：
        ① 正好两次请求 —— 断一次、自己重试一次；
        ② 第二次带了 `Range`，起点 = 第一次真的落到盘上的字节数。 */
        let reqs = reqs.lock().unwrap();
        assert_eq!(
            reqs.len(),
            2,
            "该是「断一次 → 重试一次」，实际发了 {} 次请求",
            reqs.len()
        );
        assert!(
            range_start(&reqs[0]).is_none(),
            "第一次不该带 Range：{}",
            reqs[0]
        );
        assert_eq!(
            range_start(&reqs[1]),
            Some(zip.len() / 2),
            "重试的起点不是断点（= 从头下了）：{}",
            reqs[1]
        );
        drop(reqs);

        // 进度回调也得把「已有字节」报上去，界面才不会在重试时跳回 0
        let ticks = ticks.lock().unwrap();
        assert!(
            ticks
                .iter()
                .any(|(got, _, _)| *got == (zip.len() / 2) as u64),
            "进度回调里没出现过断点（{ticks:?}）"
        );
        /* 而且最后报的必须是**在解压**：解压的分母（各条目压缩后大小之和）跟整包
        字节数差不多大，界面就靠这个字段把标签从「正在下载…」换成「正在解压…」——
        不换的话条子归零重爬，看着像下完又自动重下了一遍。 */
        assert!(
            ticks.iter().any(|(_, _, s)| *s == Stage::Download),
            "下载那一段一次都没报过（{ticks:?}）"
        );
        assert_eq!(
            ticks.last().map(|(_, _, s)| *s),
            Some(Stage::Extract),
            "最后一段该是「在解压」（{ticks:?}）"
        );

        let _ = std::fs::remove_dir_all(&dest);
    }

    /// ★ 盘上那个 `.part` 已经是**一整包**时：一个字节都别再要，直接解压。
    ///
    /// 不这么做的后果：下完 → 解压到一半关窗口 → `.part` 里躺着一整包 → 再点
    /// 「继续下载」会带着 `Range: bytes=<整包长>-` 再发一次请求（服务端不认
    /// Range 就从头下一遍）。判据见 `part_is_whole_zip`。
    #[tokio::test]
    async fn a_part_that_is_already_a_whole_zip_is_extracted_without_downloading() {
        let inner = "BS-Roformer-SW".repeat(400);
        let zip = make_zip(&[
            ("models/a.onnx", inner.as_bytes(), true),
            ("models/b.ckpt", &[7u8; 3000], false),
        ]);
        // 假服务器：真发出请求就会被记下来，而这条测试要求它**一次都没被碰**
        let (url, reqs) = half_then_resume_server(zip.clone());

        let dest = std::env::temp_dir().join("vss-svsep-whole-part-test");
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(models_dir(&dest)).unwrap();
        // 把一整包摆在 `.part` 上、记号也写好：就是「下完了、解压没跑完」那个现场
        let part = models_dir(&dest).join("svsep-models.zip.part");
        std::fs::write(&part, &zip).unwrap();
        let _ = crate::download::write_url_marker(&part, &url);

        static PAUSE: AtomicBool = AtomicBool::new(false);
        static STOP: AtomicBool = AtomicBool::new(false);
        let ctl = DownloadCtl::new(&PAUSE, &STOP, None);
        let b = Bundle::models(&dest, Some(&url));
        let out = fetch_bundle(&b, &ctl, &|_, _, _| {})
            .await
            .expect("一整包躺在盘上，该直接解开而不是再下一次");

        assert!(matches!(out, FetchOutcome::Done(_)), "没解开：{out:?}");
        let hits = reqs.lock().unwrap().len();
        assert_eq!(hits, 0, "整包都在盘上了还去发请求了（{hits} 次）");
        let models = models_dir(&dest);
        assert_eq!(
            std::fs::read(models.join("a.onnx")).unwrap(),
            inner.as_bytes(),
            "解出来的内容不对"
        );
        assert!(!part.exists(), "解完了 `.part` 还在（下次会被当成半个包）");
        assert!(!url_marker(&part).exists(), "解完了 `.part.url` 还在");

        let _ = std::fs::remove_dir_all(&dest);
    }

    /// 「一整包」的判据不能把半截包也认成一整包 —— 认错了就是拿坏 zip 去解压。
    #[test]
    fn only_a_whole_zip_counts_as_a_whole_zip() {
        let inner = "BS-Roformer-SW".repeat(400);
        let zip = make_zip(&[("models/a.onnx", inner.as_bytes(), true)]);
        let dir = std::env::temp_dir().join("vss-svsep-whole-zip-check");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.part");

        assert!(!part_is_whole_zip(&p), "文件都不在，怎么算得上一整包");
        std::fs::write(&p, b"").unwrap();
        assert!(!part_is_whole_zip(&p), "空文件不能算一整包");
        std::fs::write(&p, &zip[..zip.len() / 2]).unwrap();
        assert!(!part_is_whole_zip(&p), "下了一半的包被当成了一整包");
        // 尾巴被切掉也不行：中央目录在文件末尾，少一个字节就读不出来
        std::fs::write(&p, &zip[..zip.len() - 8]).unwrap();
        assert!(!part_is_whole_zip(&p), "少了尾巴的包被当成了一整包");
        std::fs::write(&p, &zip).unwrap();
        assert!(part_is_whole_zip(&p), "完整的一整包没认出来（那就白下了）");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5 轮都不成才报错，而且**半个包留着** —— 界面上还能点「继续下载」接着下。
    ///
    /// ⚠️ 这条要跑满 5 × 10 秒（真的等），所以标 `#[ignore]`：它验的是「放弃之后
    /// 的收场」，不是每次改下载代码都要过一遍的东西。跑法：
    ///     cargo test --bins -- --ignored gives_up_after_five_rounds --nocapture
    #[tokio::test]
    #[ignore = "要真等 50 秒（5 轮 × 10 秒），按需手动跑"]
    async fn gives_up_after_five_rounds_and_keeps_the_half_pack() {
        /// 只会掐线、永远不给数据的服务器
        fn dead_server() -> String {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                for conn in listener.incoming() {
                    let Ok(mut s) = conn else { continue };
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf);
                    // 每个响应都声明 1000 字节、只给 100 字节，然后关掉
                    let _ = s.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n",
                    );
                    let _ = s.write_all(&[1u8; 100]);
                    let _ = s.flush();
                }
            });
            format!("http://127.0.0.1:{port}/x.zip")
        }

        let url = dead_server();
        let dest = std::env::temp_dir().join("vss-svsep-retry-giveup");
        let _ = std::fs::remove_dir_all(&dest);
        static PAUSE: AtomicBool = AtomicBool::new(false);
        static STOP: AtomicBool = AtomicBool::new(false);
        let ctl = DownloadCtl::new(&PAUSE, &STOP, None);
        let b = Bundle::models(&dest, Some(&url));

        let t = Instant::now();
        let err = fetch_bundle(&b, &ctl, &|_, _, _| {}).await.unwrap_err();
        let took = t.elapsed();
        println!("放弃用了 {took:?}，报错：{err}");
        assert!(err.contains("试了 5 轮"), "报错里该说清试了几轮：{err}");
        assert!(
            took >= Duration::from_secs(5 * 10),
            "5 轮 × 10 秒没等够就放弃了：{took:?}"
        );
        // 半个包留着：界面上「继续下载」还有得续
        let part = Bundle::models(&dest, None)
            .dest
            .join("svsep-models.zip.part");
        assert!(file_size(&part) > 0, "放弃之后半个包也没了，下次只能从头下");
        assert_eq!(
            resume_point(&dest, "models", &url),
            Some(file_size(&part)),
            "留下的半个包认不出来（`resume_point` 拿不到断点）"
        );

        let _ = std::fs::remove_dir_all(&dest);
    }

    /// 把**真的** runtime 包整个解一遍（4.9 GB / 两万多个条目）。
    /// 单测造的 zip 太小，证明不了大包；而 runtime 的解压目标是程序目录，
    /// 从界面上试会覆盖正在用的运行时，所以这里用环境变量把目标指到临时目录。
    ///
    ///     $env:VSS_REAL_RUNTIME_ZIP='H:\工作站\资料归档\runtime.zip'; cargo test --bins real_runtime -- --nocapture
    #[test]
    fn extracts_the_whole_real_runtime_pack_when_asked() {
        let Ok(zip) = std::env::var("VSS_REAL_RUNTIME_ZIP") else {
            return;
        };
        // ⚠️ 解压目标必须能挑：整套 runtime 解出来 7.5 GB，`%TEMP%` 在系统盘上，
        //    撑满之后报的是 `解压失败：磁盘空间不足 (os error 112)` —— 那是**环境**
        //    的失败，但看着很像解析器有问题。所以用 VSS_REAL_RUNTIME_DEST 指到别的盘。
        let dest = std::env::var("VSS_REAL_RUNTIME_DEST")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("vss-svsep-runtime-real"));
        let _ = std::fs::remove_dir_all(&dest);

        let t = Instant::now();
        // strip 是空串 —— runtime 包必须**留住** `runtime/` 这一层
        let rep = extract_zip(Path::new(&zip), &dest, "", |_, _| {}).unwrap();
        println!(
            "解出 {} 个文件 / {} 字节，耗时 {:?}",
            rep.files,
            rep.bytes,
            t.elapsed()
        );

        assert!(rep.files > 20_000, "文件数不对：{}", rep.files);
        // 「运行时装好了没」的判据就是这两个文件（svsep.rs::runtime_ready）——
        // 所以它们必须在**同一个包**里：只有 runtime\ 而没有 backend\ 的包，
        // 用户下完几 GB 仍然起不来。
        assert!(
            dest.join("runtime").join("python.exe").is_file(),
            "runtime/python.exe 没到位"
        );
        assert!(
            dest.join("backend").join("app.py").is_file(),
            "backend/app.py 没到位"
        );
        // 分离引擎自己要用 ffmpeg（backend\config.py 会把 <svsep>\bin 塞进 PATH）
        assert!(
            dest.join("bin").join("ffmpeg.exe").is_file(),
            "bin/ffmpeg.exe 没到位"
        );
        // 大文件也要完整：torch 的包体在 runtime\Lib\site-packages 下
        assert!(
            dest.join("runtime")
                .join("Lib")
                .join("site-packages")
                .is_dir(),
            "runtime/Lib/site-packages 没解出来"
        );
        // 抽查一个**偏移超过 4 GiB** 的条目（它的 lho 是 Zip64 哨兵）——
        // 只认「大小溢出」不认「偏移溢出」的实现就是在这儿炸的。
        let torch = dest
            .join("runtime")
            .join("Lib")
            .join("site-packages")
            .join("torch")
            .join("lib")
            .join("torch_cpu.lib");
        assert!(torch.is_file(), "torch_cpu.lib 没到位（Zip64 偏移那条路）");
        assert!(
            std::fs::metadata(&torch)
                .map(|m| m.len() > 1_000_000)
                .unwrap_or(false),
            "torch_cpu.lib 解出来是空的或太小"
        );

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn extract_zip_refuses_to_escape_the_destination() {
        let base = std::env::temp_dir().join("vss-svsep-zip-slip");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let zip_path = base.join("t.zip");
        std::fs::write(
            &zip_path,
            make_zip(&[
                ("models/..\\..\\evil.txt", "pwned".as_bytes(), false), // zip slip：反斜杠也要认
                ("models/ok.txt", b"fine", false),
            ]),
        )
        .unwrap();

        let err = extract_zip(&zip_path, &base.join("out"), "models/", |_, _| {})
            .expect_err("带 .. 的条目必须直接报错");
        assert!(err.contains("不安全"), "错误文案要说得清：{err}");
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── 下载的暂停 / 停止 ──────────────────────────────────────────

    #[test]
    fn download_ctl_stops_on_either_flag_and_tells_them_apart() {
        static P: AtomicBool = AtomicBool::new(false);
        static C: AtomicBool = AtomicBool::new(false);

        let ctl = DownloadCtl::new(&P, &C, Some("  https://a/b.zip  ".into()));
        // 构造时把两个旗标都清了 —— 上一轮下载留下的 true 不能让这一轮立刻收手
        assert!(!ctl.check());
        assert!(!ctl.paused());
        assert!(!ctl.cancelled());
        // 链接两边的空格要去掉，否则 Range 请求头里会带上一段空白
        assert_eq!(ctl.resume_url.as_deref(), Some("https://a/b.zip"));

        P.store(true, Ordering::Relaxed);
        assert!(ctl.check() && ctl.paused() && !ctl.cancelled());

        // 两个都立着时按「停止」算：停止更彻底（要删 .part），宁可多删不可少删
        C.store(true, Ordering::Relaxed);
        assert!(ctl.check() && !ctl.paused() && ctl.cancelled());

        // 空链接 = 不许续传（调用方没传链接），不能变成一个空串 Range
        let ctl2 = DownloadCtl::new(&P, &C, Some("".into()));
        assert_eq!(ctl2.resume_url.as_deref(), Some(""));
        assert_eq!(DownloadCtl::new(&P, &C, None).resume_url, None);
    }

    #[test]
    fn delete_dependencies_clears_both_dirs_and_the_half_downloaded_zip() {
        /* 摆出真实布局：扩展包根（= 可写目录，`extDir` 没配置时就是这个）下面是
        `svsep/{models,runtime,bin}`；`models/` 与 `runtime/` 是**两个**目标目录，
        删错一个都不会报错，只会在用户点「开始分离」时才现形。
        ⚠️ 程序目录（`root`）在这里**故意不放东西**：它一个字节都不该被这个按钮碰到。 */
        let base = std::env::temp_dir().join("vss-svsep-del-test");
        // ⚠️ 先清干净再摆：留着上一轮跑剩的文件时，个数断言会随上一次成不成而变。
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        let writable = base.join("writable");
        let svsep = writable.join("svsep");
        let models = svsep.join("models");
        std::fs::create_dir_all(models.join("sub")).unwrap();
        std::fs::create_dir_all(svsep.join("runtime").join("Lib")).unwrap();
        std::fs::create_dir_all(svsep.join("bin")).unwrap();
        // 随程序打包的分离后端 + 用户的输出目录：**删依赖时一个都不该动**
        std::fs::create_dir_all(svsep.join("outputs")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();

        std::fs::write(models.join("BS-Roformer-SW.ckpt"), vec![7u8; 4096]).unwrap();
        std::fs::write(models.join("sub").join("x.yaml"), b"y").unwrap();
        // 没下完的半个 zip：模型的落在 models/ 里，运行时的落在 svsep/ 那一层
        std::fs::write(models.join("svsep-models.zip.part"), vec![0u8; 2048]).unwrap();
        std::fs::write(svsep.join("svsep-runtime.zip.part"), vec![0u8; 8192]).unwrap();
        std::fs::write(svsep.join("runtime").join("python.exe"), vec![0u8; 1024]).unwrap();
        std::fs::write(
            svsep.join("runtime").join("Lib").join("a.dll"),
            vec![0u8; 512],
        )
        .unwrap();
        std::fs::write(svsep.join("bin").join("ffmpeg.exe"), vec![0u8; 64]).unwrap();
        // 这一份**不该**被删：随程序打包的分离后端
        std::fs::write(svsep.join("app.py"), b"print(1)").unwrap();
        std::fs::write(svsep.join("config.py"), b"X = 1").unwrap();

        let mut last = (0u64, 0u64);
        let v = delete_dependencies(&writable, || false, |f, b| last = (f, b));

        let files = v.get("removedFiles").and_then(|x| x.as_u64()).unwrap();
        let bytes = v.get("removedBytes").and_then(|x| x.as_u64()).unwrap();
        // 7 个文件 = 模型 2 + models 里的半个 zip 1 + runtime 2 + bin 1 + svsep 里的
        // 半个 zip 1。摆进 base 的文件一共就这 7 个，删完正好一个不剩 —— 所以这个
        // 数同时也是「有没有漏删」的判据；多一个就说明 walk 把同一个文件数了两遍。
        assert_eq!(
            files, 7,
            "模型 2 + models 里的 .part 1 + runtime 2 + bin 1 + svsep 里的 .part 1，实际：{v}"
        );
        assert_eq!(bytes, 4096 + 1 + 2048 + 1024 + 512 + 64 + 8192);
        assert_eq!(last, (7, bytes), "最后一次进度回调要是最终值");
        assert_eq!(v.get("lockedCount").and_then(|x| x.as_u64()), Some(0));

        // 目录本身留着（`models/` 是引擎的 MODEL_DIR，删了它会以为没装）
        assert!(models.is_dir(), "models 目录不该被删掉");
        assert!(svsep.join("runtime").is_dir());
        // ★ 随程序打包的那些**一个都不能少**：backend 的 .py、以及运行期目录
        assert!(svsep.join("app.py").is_file(), "backend 的 .py 不该被删");
        assert!(svsep.join("config.py").is_file());
        assert!(svsep.join("outputs").is_dir(), "用户的输出目录不该被删");
        assert!(
            !svsep.join("svsep-runtime.zip.part").exists(),
            "运行时那半个 zip 要清掉"
        );

        // 再删一次：目录都空了，不能再报出个数来（否则按钮会一直说「已删 5 个」）
        let v2 = delete_dependencies(&writable, || false, |_, _| {});
        assert_eq!(v2.get("removedFiles").and_then(|x| x.as_u64()), Some(0));

        // 用户按了停止：一个都不删
        std::fs::write(models.join("again.onnx"), b"z").unwrap();
        let v3 = delete_dependencies(&writable, || true, |_, _| {});
        assert_eq!(v3.get("cancelled").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(v3.get("removedFiles").and_then(|x| x.as_u64()), Some(0));
        assert!(models.join("again.onnx").is_file());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resume_point_reads_the_disk_and_matches_the_url() {
        // 「重启之后还认不认那半个包」全靠这个函数 —— 它必须看盘上的字节，
        // 而不是任何内存里的记号（进程重启后记号就没了）。
        let base = std::env::temp_dir().join("vss-svsep-resume-test");
        let _ = std::fs::remove_dir_all(&base);
        let writable = base.join("writable");
        std::fs::create_dir_all(writable.join("svsep").join("models")).unwrap();
        let url = "https://example.test/svsep-models.zip";

        // ① 什么都没有：不能续
        assert_eq!(resume_point(&writable, "models", url), None);
        // 未知的种类（拼错 kind 不该 panic，也不该乱指一个目录）
        assert_eq!(part_path(&writable, "nope"), None);

        // ② 只有半个包、没有记号：不认。`.part` 只有字节没有出处，拿它接一个
        //    别的链接的 Range 会拼出坏 zip（要到解压才炸）。
        let part = part_path(&writable, "models").unwrap();
        assert_eq!(
            part,
            writable
                .join("svsep")
                .join("models")
                .join("svsep-models.zip.part"),
            "落点要跟着 Bundle 走，不能自己拼"
        );
        std::fs::write(&part, vec![0u8; 1234]).unwrap();
        assert_eq!(
            resume_point(&writable, "models", url),
            Some(1234),
            "记号缺失（老版本留的半个包）算能续：为了几十字节的记号丢掉几个 GB 是坏交易"
        );
        // 补上记号之后还是同一个答案
        crate::download::write_url_marker(&part, url).unwrap();
        assert_eq!(resume_point(&writable, "models", url), Some(1234));

        // ③ 记号写着**别的**链接：不认 —— 这才是那个记号存在的理由
        crate::download::write_url_marker(&part, "https://other.test/svsep-models.zip").unwrap();
        assert_eq!(resume_point(&writable, "models", url), None);

        // ④ 记号是空文件（写到一半被杀）：也当「对不上」，宁可从零下
        std::fs::write(url_marker(&part), b"").unwrap();
        assert_eq!(resume_point(&writable, "models", url), None);

        // ⑤ 收场后清记号：半个包不再算数
        crate::download::write_url_marker(&part, url).unwrap();
        clear_resume_marker(&writable, "models");
        assert!(!url_marker(&part).exists());
        assert_eq!(resume_point(&writable, "models", url), Some(1234));

        // ⑥ 空文件不算数：续到 0 字节等于没续，还得白跑一次 Range 请求
        std::fs::write(&part, b"").unwrap();
        assert_eq!(resume_point(&writable, "models", url), None);

        let _ = std::fs::remove_dir_all(&base);
    }
}
