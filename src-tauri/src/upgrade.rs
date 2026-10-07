//! 升级到新版本时，把**上一个版本留下的多余东西**清掉，免得和新版本打架。
//!
//! ## 清什么、为什么
//!
//! 新版本换了落点之后，老落点里的半成品不会再被任何人认领，而它们正好是几 GB：
//!
//! ```text
//! <老目录>/svsep/svsep-runtime.zip.part   下了一半的运行时（可能 4 GB）
//! <老目录>/svsep/svsep-models.zip.part    下了一半的模型
//! <老目录>/svsep/svsep-dml.whl            DirectML 加速包，解完就没用了，早期版本会留下
//! ```
//!
//! 更糟的是 `.part` **旁边那个记号还在**：用户下次点「继续安装」时，新版本会去认这个
//! 半个包（见 `svsep::resume_point`），把新链接的字节接到旧文件后面 —— 拼出来的 zip
//! 要到解压时才炸，而那时已经白下了几个 GB。
//!
//! ⚠️ **只删我们自己的临时文件**（`*.part` / `*.part.url` / `*.whl` / 残留的任务临时目录）。
//! 模型、运行时、配置、用户分离出来的音频**一律不碰** —— 那些重下一次是几个 GB，
//! 而删错一个就是用户的东西没了。这条边界比「删干净」重要得多。
//!
//! ⚠️ 注册表里那些**重复的卸载项 / 安装目录键**不在这里删：它们属于 HKLM、要管理员权限，
//! 而且由 MSI 的 `MajorUpgrade` 负责（见 `tauri.conf.json`）。我们能做的是**不去碰**它们。

use std::path::{Path, PathBuf};

/// 一次清理的结果（只用于写日志）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// 删掉的文件数
    pub files: u64,
    /// 删掉的字节数
    pub bytes: u64,
}

impl Report {
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }
}

/// 这些后缀的文件是**我们自己的**临时产物，任何版本都不该长期留着。
const TEMP_SUFFIXES: &[&str] = &[".part", ".part.url", ".whl"];

/// 名字里带这些片段的**顶层文件**算是我们的残留（`.part` 之外的几种）。
const TEMP_MARKERS: &[&str] = &["svsep-runtime.zip", "svsep-models.zip", "game-models.part"];

/// 扫一个目录（不递归）里的临时文件并删掉。
///
/// ⚠️ **不递归**：扩展包根下面是用户几十 GB 的正式产物（`runtime/`、`models/`），
/// 递归进去只会增加误删的面积，而我们要清的那几个文件都躺在 `svsep/`、`midi/`、
/// `svsep/models/` 这几层的**顶层**。
fn sweep_dir(dir: &Path, report: &mut Report) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if !p.is_file() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_temp(name) {
            continue;
        }
        let size = ent.metadata().map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(&p).is_ok() {
            report.files += 1;
            report.bytes += size;
        }
    }
}

/// 这个名字是不是我们的临时产物。
fn is_temp(name: &str) -> bool {
    if TEMP_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return true;
    }
    TEMP_MARKERS.iter().any(|m| name.starts_with(m))
}

/// 清理一个扩展包根下的残留。`ext` 是**那一个**根（不是可写目录）。
pub fn sweep_ext_dir(ext: &Path) -> Report {
    let mut report = Report::default();
    for sub in ["svsep", "svsep/models", "midi", "game", "game/models"] {
        sweep_dir(&ext.join(sub), &mut report);
    }
    report
}

/// 升级时该扫的**所有**落点。
///
/// 两个根都要扫：`extDir` 没配置时扩展包根就是可写目录（老行为），而用户改过配置之后
/// 老落点里那几 GB 的半个包还在原地 —— 它不会再被任何人认领，正是要清掉的那一类。
pub fn roots_to_sweep(writable: &Path, ext: &Path, previous: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let push = |p: PathBuf, out: &mut Vec<PathBuf>| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    push(writable.to_path_buf(), &mut out);
    push(ext.to_path_buf(), &mut out);
    if let Some(p) = previous {
        push(p.to_path_buf(), &mut out);
    }
    out
}

/// 跑一次清理：扫 [`roots_to_sweep`] 那几个根，删掉自己的临时文件。
///
/// 返回删了多少（给日志）。
pub fn cleanup(writable: &Path, ext: &Path, previous: Option<&Path>) -> Report {
    let mut report = Report::default();
    for root in roots_to_sweep(writable, ext, previous) {
        let r = sweep_ext_dir(&root);
        report.files += r.files;
        report.bytes += r.bytes;
    }
    if !report.is_empty() {
        crate::log_line(&format!(
            "升级清理：删掉 {} 个上一版留下的临时文件（{:.1} MB）",
            report.files,
            report.bytes as f64 / 1048576.0
        ));
    }
    report
}

/* ══════════════════════════════ 旧配置键 ══════════════════════════════ */

/// 把**老版本**留下的配置键收敛到新键上，返回改没改。
///
/// 现在只有一个：`svsepRuntimeDir` 是上一版给音轨分离单独开的落点，新版本统一成
/// `extDir`。两个键同时留着不会报错，但会让「设置页显示 A、实际落 B」这种鬼状态
/// 出现 —— 所以启动时对齐一次，之后只认 `extDir`。
///
/// ⚠️ **只搬不改值**：用户当初选的目录原样搬过去，不替他做决定。
pub fn migrate_config(cfg: &mut serde_json::Value) -> bool {
    let Some(map) = cfg.as_object_mut() else {
        return false;
    };
    let ext_empty = map
        .get("extDir")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().is_empty())
        .unwrap_or(true);
    let legacy = map
        .get("svsepRuntimeDir")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match (ext_empty, legacy) {
        (true, Some(old)) => {
            map.insert("extDir".into(), serde_json::json!(old));
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vss-upgrade-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 只删自己的临时文件；模型、运行时、配置、用户的产物一个都不许动。
    ///
    /// 这条钉的是一条**不能出错**的边界：删错一个就是用户几个 GB 的下载或他自己的东西没了。
    #[test]
    fn sweep_removes_only_our_temp_files() {
        let ext = tmp("sweep");
        let svsep = ext.join("svsep");
        let models = svsep.join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::create_dir_all(ext.join("midi")).unwrap();

        // 该删的
        std::fs::write(svsep.join("svsep-runtime.zip.part"), vec![0u8; 1000]).unwrap();
        std::fs::write(svsep.join("svsep-runtime.zip.part.url"), b"u").unwrap();
        std::fs::write(svsep.join("svsep-dml.whl"), vec![0u8; 500]).unwrap();
        std::fs::write(models.join("svsep-models.zip.part"), vec![0u8; 250]).unwrap();
        std::fs::write(ext.join("midi").join("game-models.part"), vec![0u8; 10]).unwrap();

        // 一个都不该碰的
        std::fs::write(models.join("BS-Roformer-SW.ckpt"), b"weights").unwrap();
        std::fs::write(svsep.join("backend.py"), b"code").unwrap();
        std::fs::write(ext.join("config.json"), b"{}").unwrap();
        std::fs::create_dir_all(svsep.join("outputs")).unwrap();
        std::fs::write(svsep.join("outputs").join("vocals.wav"), b"audio").unwrap();
        // 用户的音频叫 `xxx.wav.part` 的可能性存在，但它不在我们扫的那几个目录里
        std::fs::create_dir_all(svsep.join("uploads")).unwrap();
        std::fs::write(svsep.join("uploads").join("song.part"), b"audio").unwrap();

        let r = sweep_ext_dir(&ext);
        assert_eq!(r.files, 5, "只该删那 5 个临时文件：{r:?}");
        assert_eq!(r.bytes, 1000 + 1 + 500 + 250 + 10);

        assert!(models.join("BS-Roformer-SW.ckpt").is_file(), "模型不许动");
        assert!(svsep.join("backend.py").is_file());
        assert!(ext.join("config.json").is_file());
        assert!(svsep.join("outputs").join("vocals.wav").is_file());
        assert!(
            svsep.join("uploads").join("song.part").is_file(),
            "uploads 里的东西不是我们的临时文件，不该被扫到"
        );

        // 再扫一次：没得删了（幂等，不会越删越多）
        assert!(sweep_ext_dir(&ext).is_empty());
        let _ = std::fs::remove_dir_all(&ext);
    }

    /// 换过目录的用户，老落点也要扫 —— 那几 GB 的半个包就在那儿。
    #[test]
    fn old_and_new_roots_are_both_swept_without_duplicates() {
        let writable = tmp("roots-writable");
        let ext = tmp("roots-ext");
        let old = tmp("roots-old");
        std::fs::create_dir_all(old.join("svsep")).unwrap();
        std::fs::write(old.join("svsep").join("svsep-runtime.zip.part"), b"half").unwrap();

        let roots = roots_to_sweep(&writable, &ext, Some(&old));
        assert_eq!(roots.len(), 3);
        // 没配 `extDir` 时前两个是同一个目录，去重后只留一个
        let same = roots_to_sweep(&writable, &writable, None);
        assert_eq!(same.len(), 1, "同一个目录不该扫两遍（会把计数算重）");

        let r = cleanup(&writable, &ext, Some(&old));
        assert_eq!(r.files, 1);
        assert!(!old.join("svsep").join("svsep-runtime.zip.part").exists());
        let _ = std::fs::remove_dir_all(&writable);
        let _ = std::fs::remove_dir_all(&ext);
        let _ = std::fs::remove_dir_all(&old);
    }

    /// 老配置键收敛到新键：只在**新键还空着**时搬，用户已经选了就不覆盖。
    #[test]
    fn legacy_dir_key_moves_to_the_new_one_once() {
        let mut cfg = serde_json::json!({"extDir": "", "svsepRuntimeDir": "D:\\VSS"});
        assert!(migrate_config(&mut cfg));
        assert_eq!(cfg["extDir"], serde_json::json!("D:\\VSS"));
        // 再跑一次不该再改（幂等）
        assert!(!migrate_config(&mut cfg));

        // 新键已经填了 → 不覆盖（那是用户在本版里选的）
        let mut cfg2 = serde_json::json!({"extDir": "E:\\new", "svsepRuntimeDir": "D:\\old"});
        assert!(!migrate_config(&mut cfg2));
        assert_eq!(cfg2["extDir"], serde_json::json!("E:\\new"));

        // 两边都空 → 什么都不做
        let mut cfg3 = serde_json::json!({"extDir": "", "svsepRuntimeDir": "  "});
        assert!(!migrate_config(&mut cfg3));
    }
}
