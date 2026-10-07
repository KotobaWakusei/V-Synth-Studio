//! 「检查更新」的命令。
//!
//! ⚠️ 上游没发过 Release、GitHub 匿名调用超额都是 `ok: false` 的**成功回包**，
//! 只有真连不上才回 `Err` —— 两种在界面上都只是一句话，见 `crate::update`。

use super::Cmd;

/// 问一次 GitHub 上最新的 release（见 `crate::update`）。
#[tauri::command]
pub async fn update_check() -> Cmd {
    crate::update::check().await
}
