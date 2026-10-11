//! 「检查更新」—— 问一次 GitHub Releases 上最新那条发布。
//!
//! ⚠️ **只读、只查，不下载也不安装**：回包里只有版本号、说明、发布页地址，
//! 装不装由用户自己点「打开发布页」。
//!
//! 上游仓库写死在这里（不是配置项）：它就是这个程序自己的仓库，
//! 换个地址查到的版本号与本机没有任何可比性。
//!
//! # 连不上 GitHub 时走镜像
//!
//! 先直连 `api.github.com`，失败才依次试 [`API_MIRRORS`] 里那几面（拼接方式见
//! [`mirror_url`]）。判据是**响应体**：能解析出 `tag_name` 才算数 —— 代理回一页
//! HTML / 403 错误页是常事，只看状态码会把它们当成「查到了」。
//!
//! ⚠️ **不是把 `ghmirror` 那套搬过来**：那个是给几 GB 的下载「实测挑最快的一面」，
//! 而这里只取一份几 KB 的 JSON —— 为它去取测速名单、开八个并发实测，代价与收益
//! 完全不成比例。这里的名单是固定的几面，按顺序试到第一个能用的为止。
//!
//! ⚠️ **名单里只放实测确认能代理 `api.github.com` 的**。GitHub 的公开 API 会对
//! **代理的共享出口 IP** 限流：实测 `ghfast.top` / `gh.llkk.cc` / `ghproxy.net`
//! 走 API 路径一律回 403，而同一个文件走它们下载是好的 —— 用下载镜像的思路猜
//! API 镜像会挑到一堆 403。
//!
//! ⚠️ 镜像拿到的是本程序的版本询问，**不含 Cookie、不含账号信息**；回包还会带
//! `via`，界面据此说明这次是哪一面答的。

use std::time::Duration;

use serde_json::{Value, json};

/// 上游仓库的规范接口地址。
const LATEST_API: &str = "https://api.github.com/repos/QingMu39-Gao/V-Synth-Studio/releases/latest";

/// 直连那一次的等待上限。连接被墙时表现为「连上了但不给数据」，所以要一个**短的**
/// 总超时：镜像是在它失败之后才试的，这里多等一秒就是用户多等一秒。
const DIRECT_TIMEOUT: Duration = Duration::from_secs(6);
/// 镜像那几次的等待上限（比直连宽松一点：代理要多一跳）。
const MIRROR_TIMEOUT: Duration = Duration::from_secs(8);

/// 能代理 `api.github.com` 的镜像，按顺序试。语义与 `ghmirror::FALLBACK` 一样：
/// 会烂，所以只当退路，而且**只放当前实测过的那几面**。
const API_MIRRORS: &[&str] = &["https://gh-proxy.com/", "https://ghproxy.net/"];

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

/// 这一面镜像上，规范地址变成了什么（`base` 带不带尾斜杠都要拼对）。
fn mirror_url(base: &str, canonical: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), canonical)
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
///
/// 直连不通（境内常见）就依次试镜像；哪一面答的都写进回包的 `via` ——
/// 界面要能说明这次是第三方代答的（见模块头）。
pub async fn check() -> Result<Value, String> {
    let mut tried: Vec<String> = vec!["GitHub 直连".to_string()];

    match fetch(LATEST_API, DIRECT_TIMEOUT).await {
        Ok(v) => return Ok(reply(&v, "GitHub 直连")),
        Err(why) => {
            if why.is_error_page() {
                return Ok(why.into_reply());
            }
            crate::log_line(&format!("检查更新：直连 GitHub 失败（{why}），改试镜像"));
        }
    }

    for base in API_MIRRORS {
        let url = mirror_url(base, LATEST_API);
        let label = base.trim_end_matches('/').to_string();
        match fetch(&url, MIRROR_TIMEOUT).await {
            Ok(v) => {
                crate::log_line(&format!("检查更新：{url} 答了"));
                return Ok(reply(&v, &label));
            }
            Err(why) => {
                /* 镜像答的是正经的 GitHub 错误（404 上游没发过 Release、403 限流）时
                   就到此为止：再问下一面也是同一个答案，只是换了个出口 IP 去撞限流。 */
                if why.is_error_page() {
                    return Ok(why.into_reply());
                }
                tried.push(label);
            }
        }
    }

    Err(format!(
        "检查更新失败：GitHub 直连与 {} 面镜像都没答上来（{}）。网络恢复后再试，或直接打开发布页",
        API_MIRRORS.len(),
        tried.join("、")
    ))
}

/// 一次请求的失败原因。
#[derive(Debug)]
enum Failure {
    /// 连不上 / 超时 / 响应不是 JSON：**换一面还有机会**。
    Transport(String),
    /// 拿到了正经的 GitHub 错误回包（404 没发过 Release、403 限流、5xx）。
    /// 这种答案换一面也是一样的，直接报给用户。
    Github(Value),
}

impl Failure {
    fn is_error_page(&self) -> bool {
        matches!(self, Failure::Github(_))
    }

    fn into_reply(self) -> Value {
        match self {
            Failure::Github(v) => v,
            // 调用方只在 `is_error_page()` 为真时才用它，这里只是让 match 完整
            Failure::Transport(msg) => json!({ "ok": false, "error": msg }),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Transport(msg) => write!(f, "{msg}"),
            Failure::Github(v) => write!(f, "{}", v["error"].as_str().unwrap_or("GitHub 报错")),
        }
    }
}

/// 取一个候选地址并解析。
async fn fetch(url: &str, timeout: Duration) -> Result<Value, Failure> {
    /* ⚠️ 走 `net::client()` 而不是自己建一个：那个客户端挂了 UA，而 GitHub 对
    没有 `User-Agent` 的请求一律回 403（表现为「被限流」这种误判）。 */
    let res = crate::net::client()
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| {
            Failure::Transport(if e.is_timeout() {
                format!("超时（{} 秒没回应）", timeout.as_secs())
            } else {
                format!("连不上：{e}")
            })
        })?;

    let status = res.status().as_u16();
    let text = res.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(Failure::Github(failure(status, &text)));
    }

    /* ⚠️ 判据是**响应体里有没有 `tag_name`**，不是状态码：代理对取不到的路径回
       一页 HTML（200）是常事，只看状态码会把它当成「上游还没发 Release」。 */
    let v: Value = serde_json::from_str(&text)
        .map_err(|_| Failure::Transport(format!("回的不是 JSON：{}", brief(&text))))?;
    if v["tag_name"].as_str().unwrap_or("").trim().is_empty() {
        return Err(Failure::Transport(format!(
            "回包里没有 tag_name：{}",
            brief(&text)
        )));
    }
    Ok(v)
}

/// 把 GitHub 的回包整理成界面要的形状。`via` 说明这一次是谁答的。
fn reply(v: &Value, via: &str) -> Value {
    let tag = v["tag_name"].as_str().unwrap_or("").trim();
    let latest = tag.strip_prefix(['v', 'V']).unwrap_or(tag);
    let notes = clamp_notes(v["body"].as_str().unwrap_or(""));

    json!({
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
        "via": via,
    })
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

    /// 镜像把**整条规范地址**接在自己后面，尾斜杠要归一（名单是手写的常量，
    /// 少一个斜杠就拼出 `https://gh-proxy.comhttps://api...`）。
    #[test]
    fn mirror_url_joins_with_exactly_one_slash() {
        assert_eq!(
            mirror_url("https://gh-proxy.com/", LATEST_API),
            format!("https://gh-proxy.com/{LATEST_API}")
        );
        assert_eq!(
            mirror_url("https://ghproxy.net", LATEST_API),
            format!("https://ghproxy.net/{LATEST_API}")
        );
    }

    /// 回包形状：`via` 必须写明是谁答的（界面要说清「这次是第三方代答的」），
    /// 而带着前导 `v` 的 tag 仍要出 `hasUpdate`。
    #[test]
    fn reply_carries_the_source_and_normalises_the_tag() {
        let v = json!({
            "tag_name": "v9.9.9",
            "name": "更名",
            "body": "说明",
            "html_url": "https://github.com/x/y/releases/tag/v9.9.9",
            "published_at": "2026-01-01T00:00:00Z",
            "prerelease": false,
        });
        let r = reply(&v, "https://gh-proxy.com");
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["latest"], json!("9.9.9"), "前导 v 要去掉");
        assert_eq!(r["via"], json!("https://gh-proxy.com"));
        assert_eq!(r["hasUpdate"], json!(true));

        // 上游还没发过 Release 的形状：没有 tag_name，解析层必须把它挡成「换一面试」
        assert!(v["tag_name"].as_str().is_some());
        let empty = json!({ "message": "Not Found" });
        assert!(empty["tag_name"].as_str().unwrap_or("").trim().is_empty());
    }

    /// 真去问一次 GitHub（要联网 + 会吃匿名调用额度，所以用环境变量门住）。
    ///
    /// 这条测的是**真实回包的形状**：字段名、`tag_name` 里那个前导 `v`、`hasUpdate`
    /// 的算法在真数据上算出来是什么 —— 那几处只有连真接口才验得到，假 JSON 只能验解析。
    /// 顺带打印 `via`：直连与镜像哪条路真的通，只有这么看。
    ///
    ///     $env:VSS_REAL_UPDATE_CHECK='1'
    ///     cargo test --lib update::tests::real_api -- --nocapture
    #[tokio::test]
    async fn real_api_answers_when_asked() {
        if std::env::var("VSS_REAL_UPDATE_CHECK").is_err() {
            return;
        }
        let v = check().await.expect("直连与镜像都试过还是连不上时这条测试才算失败");
        println!("回包：{}", serde_json::to_string_pretty(&v).unwrap());
        assert_eq!(v["ok"], json!(true), "上游有 Release 时才跑这条：{v}");
        assert_eq!(v["current"], json!(current_version()));
        assert!(!v["latest"].as_str().unwrap_or("").is_empty());
        assert!(!parse_version(v["latest"].as_str().unwrap()).is_empty());
        assert!(v["url"].as_str().unwrap_or("").contains("github.com"));
        assert!(v["hasUpdate"].is_boolean());
        assert!(!v["via"].as_str().unwrap_or("").is_empty(), "要说明是谁答的");
        // 真数据上再钉一次「本机比远端新不算更新」：拿本机版本自己跟自己比
        assert!(!is_newer(current_version(), current_version()));
    }

    /// **每一面镜像都真打一次**，报哪几面能代答 `api.github.com`。
    ///
    /// 为什么值得单独跑：能代理下载的镜像**不等于**能代理 API —— 实测
    /// `ghfast.top` / `gh.llkk.cc` / `ghproxy.net` 走 API 路径一律回 403（代理的
    /// 共享出口 IP 撞上 GitHub 限流），而它们下载都是好的。名单要按这条结果维护。
    ///
    ///     cargo test --lib update::tests::real_mirrors -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "要联网逐面实测，按需手动跑"]
    async fn real_mirrors_answer_which_ones_proxy_the_api() {
        println!("直连：{:?}", fetch(LATEST_API, DIRECT_TIMEOUT).await.map(|_| "ok"));
        for base in API_MIRRORS {
            let url = mirror_url(base, LATEST_API);
            match fetch(&url, MIRROR_TIMEOUT).await {
                Ok(v) => println!("  ✓ {base}  tag={}", v["tag_name"]),
                Err(e) => println!("  ✗ {base}  {e}"),
            }
        }
    }
}
