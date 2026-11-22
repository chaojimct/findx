//! 设置页的搜索与深链接逻辑。
//!
//! - `filter`：按查询过滤设置项（label + 关键词），大小写不敏感、空格归一化。
//! - `parse_deep_link`：把 "settings:diagnostics" / "settings:index#locations" 这类
//!   字符串解析成 (tab, section)，UI 据此打开设置页并滚动到对应位置。
//!
//! 与 GPUI 解耦，可无窗口测试。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsItem {
    pub id: &'static str,
    pub tab: &'static str,
    pub label: &'static str,
    pub keywords: &'static [&'static str],
    pub group: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepLink {
    pub tab: String,
    pub section: Option<String>,
}

/// 空查询返回全部；非空时按 label + keywords 子串匹配（大小写不敏感）。
pub fn filter(items: &[SettingsItem], query: &str) -> Vec<SettingsItem> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return items.to_vec();
    }
    let tokens: Vec<&str> = q.split_whitespace().collect();
    items
        .iter()
        .filter(|it| {
            let haystack = format!(
                "{} {} {}",
                it.label.to_lowercase(),
                it.tab.to_lowercase(),
                it.keywords.join(" ").to_lowercase()
            );
            tokens.iter().all(|t| haystack.contains(t))
        })
        .cloned()
        .collect()
}

/// 解析 "settings:<tab>" 或 "settings:<tab>#<section>"。tab/section 都会 trim
/// 并转小写；空 tab 返回 None。
pub fn parse_deep_link(s: &str) -> Option<DeepLink> {
    let s = s.trim();
    let rest = s
        .strip_prefix("settings:")
        .or_else(|| s.strip_prefix("findx://settings/"))
        .or_else(|| s.strip_prefix("findx:settings:"))?;
    let (tab, section) = match rest.split_once('#') {
        Some((t, sec)) => (t, Some(sec.trim().to_string())),
        None => (rest, None),
    };
    let tab = tab.trim().to_lowercase();
    if tab.is_empty() {
        return None;
    }
    let section = section
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    Some(DeepLink { tab, section })
}

/// 设置页 tab id 的规范化：别名映射到规范 tab 名。
pub fn canonical_tab(tab: &str) -> &'static str {
    match tab.trim().to_lowercase().as_str() {
        "appearance" | "theme" | "外观" => "appearance",
        "search" | "搜索" => "search",
        "files" | "file" | "文件" => "files",
        "preview" | "预览" => "preview",
        "index" | "索引" => "index",
        "shortcuts" | "keys" | "快捷键" => "shortcuts",
        "diagnostics" | "diag" | "诊断" => "diagnostics",
        "about" | "关于" => "about",
        "service" | "服务" => "service",
        "maintenance" | "维护" => "maintenance",
        _ => "appearance",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &'static str, tab: &'static str, label: &'static str, kw: &'static [&'static str]) -> SettingsItem {
        SettingsItem { id, tab, label, keywords: kw, group: tab }
    }

    #[test]
    fn empty_query_returns_all() {
        let items = vec![item("a", "index", "索引范围", &[]), item("b", "about", "关于", &[])];
        assert_eq!(filter(&items, "").len(), 2);
        assert_eq!(filter(&items, "   ").len(), 2);
    }

    #[test]
    fn multi_token_requires_all() {
        let items = vec![
            item("a", "index", "索引范围", &["磁盘"]),
            item("b", "index", "排除目录", &["路径"]),
        ];
        let out = filter(&items, "index 磁盘");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "a");
    }

    #[test]
    fn case_insensitive_substring() {
        let items = vec![item("a", "about", "About", &["version"])];
        assert_eq!(filter(&items, "ABOUT").len(), 1);
        assert_eq!(filter(&items, "VERSION").len(), 1);
        assert!(filter(&items, "nope").is_empty());
    }

    #[test]
    fn parse_deep_link_handles_both_schemes() {
        let d = parse_deep_link("settings:diagnostics").unwrap();
        assert_eq!(d.tab, "diagnostics");
        assert!(d.section.is_none());

        let d = parse_deep_link("findx://settings/index#locations").unwrap();
        assert_eq!(d.tab, "index");
        assert_eq!(d.section.as_deref(), Some("locations"));

        let d = parse_deep_link("findx:settings:about").unwrap();
        assert_eq!(d.tab, "about");
    }

    #[test]
    fn parse_deep_link_rejects_empty_tab() {
        assert!(parse_deep_link("settings:").is_none());
        assert!(parse_deep_link("not-a-link").is_none());
        assert!(parse_deep_link("   ").is_none());
    }

    #[test]
    fn canonical_tab_maps_aliases() {
        assert_eq!(canonical_tab("Index"), "index");
        assert_eq!(canonical_tab("诊断"), "diagnostics");
        assert_eq!(canonical_tab("keys"), "shortcuts");
        assert_eq!(canonical_tab("garbage"), "appearance");
    }
}