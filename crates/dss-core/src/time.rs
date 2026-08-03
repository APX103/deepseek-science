//! 时间工具：UTC，RFC3339 序列化（与 data-model 总则一致）。

/// 当前 UTC 时间的 RFC3339 字符串（带 `Z`）。
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
