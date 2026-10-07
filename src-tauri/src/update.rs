//! 「检查更新」—— 问一次 GitHub Releases 上最新那条发布。
//!
//! ⚠️ **只读、只查，不下载也不安装**：回包里只有版本号、说明、发布页地址，
//! 装不装由用户自己点「打开发布页」。
//!
//! 上游仓库写死在这里（不是配置项）：它就是这个程序自己的仓库，
//! 换个地址查到的版本号与本机没有任何可比性。

use std::time::Duration;

use serde_json::{Value, json};

const LATEST_API: &str = "https://api.github.com/repos/QingMu39-Gao/V-Synth-Studio/releases/latest";

/// 发布说明是一整篇 markdown，整份过 IPC 会让回包大得没道理。
const NOTES_LIMIT: usize = 4000;

/// 出错时给用户看一小段响应体就够了，别把整页 JSON 塞进界面。
fn brief(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() <= 200 {
        return t.to_string();
    }
    let head: String = t.chars().take(200).collect();
    format!("{head}…")
}

/// 界面要显示的版本，就是 `get_state` 发出去的那个（真源见 `ipc::config_file`）。
pub fn current_version() -> &'static str {
    crate::ipc::config_file::APP_VERSION
}

/// 把 tag / 版本串拆成数字段。
///
/// 前导 `v` 不要（GitHub 的 tag 惯例），遇到第一个非数字非 `.` 的字符就停 ——
/// 版本号后面的后缀（`1.3.3beta`）不参与比较，比较只按数字段走。
/// 一段解析不成数字也停：`1.4.x` 取 `[1,4]`，别把 `.x` 当成 0 蒙混过去。
pub fn parse_version(tag: &str) -> Vec<u64> {
    let s = tag.trim();
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    let head = s
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()
        .unwrap_or("");
    let mut parts = Vec::new();
    for part in head.split('.') {
        match part.parse::<u64>() {
            Ok(n) => parts.push(n),
            Err(_) => break,
        }
    }
    parts
}

/// 远端是否比本机新。
///
/// 逐段比数字，缺的段按 0 算（`1.10` > `1.9`，`1.4` 与 `1.4.0` 一样新）。
/// 远端一个数字都解析不出来时回 false：宁可不说有更新，也不能拿一个读不懂的
/// tag 去怂用户。本机比远端新（自编的包）同样回 false —— 见 `check`。
pub fn is_newer(remote: &str, local: &str) -> bool {
    let (r, l) = (parse_version(remote), parse_version(local));
    if r.is_empty() {
        return false;
    }
    for i in 0..r.len().max(l.len()) {
        let (a, b) = (
            r.get(i).copied().unwrap_or(0),
            l.get(i).copied().unwrap_or(0),
        );
        if a != b {
            return a > b;
        }
    }
    false
}

/// 发布说明截到 [`NOTES_LIMIT`] 个字符（按字符数，不是字节数）。
fn clamp_notes(s: &str) -> String {
    if s.chars().count() <= NOTES_LIMIT {
        return s.to_string();
    }
    let head: String = s.chars().take(NOTES_LIMIT).collect();
    format!("{head}…")
}

/// 非 2xx 时的回包。
///
/// ⚠️ **这不是 `Err`**：上游还没发过 Release（404）、匿名调用超额（403）都是
/// 正常状态，冒泡成异常会让前端只能弹一个 toast，而这几种情况都要能停在面板上
/// 让用户看清是什么事。
fn failure(status: u16, body: &str) -> Value {
    let error = match status {
        404 => "上游仓库还没有发布过 Release，暂时查不到更新".to_string(),
        403 | 429 => format!(
            "GitHub 拒绝了这次请求（HTTP {status}，匿名调用次数可能已用完，过一会儿再试）：{}",
            brief(body)
        ),
        code => format!("GitHub 回了 HTTP {code}：{}", brief(body)),
    };
    json!({ "ok": false, "error": error })
}

/// 查一次最新发布。
pub async fn check() -> Result<Value, String> {
    /* ⚠️ 走 `net::client()` 而不是自己建一个：那个客户端挂了 UA，而 GitHub 对
    没有 `User-Agent` 的请求一律回 403（表现为「被限流」这种误判）。 */
    let res = crate::net::client()
        .get(LATEST_API)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "检查更新超时：GitHub 十秒内没有回应，稍后再试".to_string()
            } else {
                format!("连不上 GitHub：{e}")
            }
        })?;

    let status = res.status().as_u16();
    let text = res.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Ok(failure(status, &text));
    }

    let v: Value = serde_json::from_str(&text)
        .map_err(|_| "GitHub 回的内容不是合法 JSON，过一会儿再试".to_string())?;
    let tag = v["tag_name"].as_str().unwrap_or("").trim();
    let latest = tag.strip_prefix(['v', 'V']).unwrap_or(tag);
    let notes = clamp_notes(v["body"].as_str().unwrap_or(""));

    Ok(json!({
        "ok": true,
        "current": current_version(),
        "latest": latest,
        // ⚠️ 本机比远端新（自编的包）时必须是 false，见 `is_newer`
        "hasUpdate": is_newer(tag, current_version()),
        "name": v["name"].as_str().unwrap_or(""),
        "notes": notes,
        "url": v["html_url"].as_str().unwrap_or(""),
        "publishedAt": v["published_at"].as_str().unwrap_or(""),
        "prerelease": v["prerelease"].as_bool().unwrap_or(false),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /* 真实回包（api.github.com）不进单测：要联网、还会因限流变脸。这里只钉住
    那几处**算错了肉眼看不出来**的地方 —— 版本号比较与两种回包形状。 */

    #[test]
    fn parse_version_stops_at_the_first_non_numeric_part() {
        assert_eq!(parse_version("v1.4.0"), vec![1, 4, 0]);
        assert_eq!(parse_version("V1.4.0"), vec![1, 4, 0]);
        // 后缀（`beta` / `-rc1`）不参与比较
        assert_eq!(parse_version("1.3.3beta"), vec![1, 3, 3]);
        assert_eq!(parse_version("v2.0.0-rc1"), vec![2, 0, 0]);
        // 段解析不出来就停：`1.4.x` 是 [1,4]，不是 [1,4,0]
        assert_eq!(parse_version("1.4.x"), vec![1, 4]);
        assert_eq!(parse_version("1.10"), vec![1, 10]);
        assert_eq!(parse_version("garbage"), Vec::<u64>::new());
        assert_eq!(parse_version(""), Vec::<u64>::new());
    }

    #[test]
    fn is_newer_compares_element_by_element() {
        assert!(is_newer("v1.4.0", "1.3.3beta"));
        // 数字段比字典序大：1.3.10 比 1.3.4 新
        assert!(is_newer("v1.3.10", "1.3.4"));
        // 缺的段按 0 算，所以 1.10 > 1.9
        assert!(is_newer("1.10", "1.9"));
        // 后缀不影响：同一个数字段就是同一个版本
        assert!(!is_newer("v1.3.3", "1.3.3beta"));
        assert!(!is_newer("v1.3.3", "1.3.3"));
        assert!(!is_newer("v1.2.0", "1.3.3"));
        // 本机比远端新（自己编的包）不算「有更新」
        assert!(!is_newer("v1.2.0", "2.0.0"));
        // 远端读不懂时宁可不说有更新
        assert!(!is_newer("garbage", "1.0.0"));
        assert!(!is_newer("", "1.0.0"));
    }

    #[test]
    fn clamp_notes_cuts_huge_bodies() {
        assert_eq!(clamp_notes("短说明"), "短说明");
        let long = "x".repeat(NOTES_LIMIT + 500);
        let cut = clamp_notes(&long);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), NOTES_LIMIT + 1);
    }

    #[test]
    fn failure_reads_as_one_sentence_without_the_whole_body() {
        let f = failure(404, "{\"message\":\"Not Found\"}");
        assert_eq!(f["ok"], json!(false));
        let msg = f["error"].as_str().unwrap();
        assert!(msg.contains("Release"), "{msg}");
        // 403 要带上「为什么被拒」，但响应体只留一小段
        let f = failure(403, &format!("{{\"message\":\"{}\"}}", "x".repeat(500)));
        let msg = f["error"].as_str().unwrap();
        assert!(msg.contains("403"), "{msg}");
        assert!(msg.chars().count() < 400, "{msg}");
        assert!(failure(500, "").to_string().contains("500"));
    }

    /// 真去问一次 GitHub（要联网 + 会吃匿名调用额度，所以用环境变量门住）。
    ///
    /// 这条测的是**真实回包的形状**：字段名、`tag_name` 里那个前导 `v`、`hasUpdate`
    /// 的算法在真数据上算出来是什么 —— 那几处只有连真接口才验得到，假 JSON 只能验解析。
    ///
    ///     $env:VSS_REAL_UPDATE_CHECK='1'
    ///     cargo test --lib update::tests::real_api -- --nocapture
    #[tokio::test]
    async fn real_api_answers_when_asked() {
        if std::env::var("VSS_REAL_UPDATE_CHECK").is_err() {
            return;
        }
        let v = check().await.expect("连不上 GitHub 时这条测试才算失败");
        println!("回包：{}", serde_json::to_string_pretty(&v).unwrap());
        assert_eq!(v["ok"], json!(true), "上游有 Release 时才跑这条：{v}");
        assert_eq!(v["current"], json!(current_version()));
        assert!(!v["latest"].as_str().unwrap_or("").is_empty());
        assert!(!parse_version(v["latest"].as_str().unwrap()).is_empty());
        assert!(v["url"].as_str().unwrap_or("").contains("github.com"));
        assert!(v["hasUpdate"].is_boolean());
        // 真数据上再钉一次「本机比远端新不算更新」：拿本机版本自己跟自己比
        assert!(!is_newer(current_version(), current_version()));
    }
}
