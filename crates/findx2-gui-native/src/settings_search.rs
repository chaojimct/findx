//! 设置页的搜索与深链接逻辑。
//!
//! - `all_settings_items`：设置项目录（id / 所属 tab / 界面文案 / 关键词 / 分组）。
//! - `filter`：按查询匹配设置项的**内容**，中文、全拼、首字母简拼都支持
//!   （复用搜索页同一套 ib-matcher 拼音表；无 pinyin feature 时退化为子串匹配）。
//! - `parse_deep_link`：把 "settings:diagnostics" / "settings:index#locations" 这类
//!   字符串解析成 (tab, section)，UI 据此打开设置页并滚动到对应位置。
//!
//! 与 GPUI 解耦，可无窗口测试。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsItem {
    pub id: &'static str,
    /// 规范 tab id（appearance/search/…），用于跳转。
    pub tab: &'static str,
    /// 界面上的真实文案。
    pub label: &'static str,
    /// 同义词 / 英文别名；中文由 ib-matcher 自动转拼音，无需手写拼音串。
    pub keywords: &'static [&'static str],
    /// 分组（卡片标题），仅用于展示与检索。
    pub group: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepLink {
    pub tab: String,
    pub section: Option<String>,
}

/// tab 规范 id → 界面上的中文标签。
pub fn tab_label(tab: &str) -> &'static str {
    match tab {
        "appearance" => "外观",
        "search" => "搜索",
        "files" => "文件浏览",
        "preview" => "预览",
        "index" => "索引与服务",
        "shortcuts" => "快捷键",
        "diagnostics" => "诊断",
        "about" => "关于",
        _ => "设置",
    }
}

/// 空查询返回全部；非空时逐 token 匹配「文案 + tab 中文名 + 关键词」。
/// 开启 pinyin feature 时，中文文案可用全拼或首字母简拼命中（与搜索页一致）。
pub fn filter(items: &[SettingsItem], query: &str) -> Vec<SettingsItem> {
    let q = query.trim();
    if q.is_empty() {
        return items.to_vec();
    }
    let tokens: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();

    #[cfg(feature = "pinyin")]
    let config = ib_matcher::matcher::MatchConfig::builder()
        .pinyin(ib_matcher::matcher::PinyinMatchConfig::notations(
            ib_matcher::pinyin::PinyinNotation::Ascii
                | ib_matcher::pinyin::PinyinNotation::AsciiFirstLetter,
        ))
        .build();
    #[cfg(feature = "pinyin")]
    let matchers: Vec<_> = tokens
        .iter()
        .map(|t| config.matcher::<str>(t.as_str()))
        .collect();

    items
        .iter()
        .filter(|it| {
            let haystack = format!(
                "{} {} {} {}",
                it.label,
                tab_label(it.tab),
                it.group,
                it.keywords.join(" ")
            );
            tokens.iter().enumerate().all(|(i, _token)| {
                #[cfg(feature = "pinyin")]
                {
                    matchers[i].find(haystack.as_str()).is_some()
                }
                #[cfg(not(feature = "pinyin"))]
                {
                    haystack.to_lowercase().contains(_token)
                }
            })
        })
        .cloned()
        .collect()
}

/// 全量设置项目录。label 与 settings_window.rs 里的界面文案保持一致。
pub fn all_settings_items() -> Vec<SettingsItem> {
    fn item(
        id: &'static str,
        tab: &'static str,
        label: &'static str,
        group: &'static str,
        keywords: &'static [&'static str],
    ) -> SettingsItem {
        SettingsItem {
            id,
            tab,
            label,
            keywords,
            group,
        }
    }

    vec![
        // 外观
        item("theme_style", "appearance", "主题风格", "主题风格", &["theme", "主题", "风格", "外观", "样式", "light", "dark"]),
        item("ui_mode", "appearance", "明暗模式", "明暗模式", &["theme", "浅色", "深色", "跟随系统", "light", "dark", "system"]),
        // 搜索
        item("pinyin_default", "search", "默认拼音匹配", "搜索", &["pinyin", "拼音", "全拼", "简拼"]),
        item("search_limit", "search", "结果上限", "搜索", &["limit", "max", "results", "条数"]),
        item("syntax_help", "search", "语法帮助", "搜索", &["syntax", "help", "语法", "帮助"]),
        // 文件浏览
        item("dir_tabs", "files", "标签", "目录", &["tab", "位置", "历史", "选中"]),
        item("dir_filter", "files", "过滤", "目录", &["filter", "名称", "当前层"]),
        item("dir_subdir", "files", "子目录", "目录", &["subdir", "下层", "递归", "搜索"]),
        item("dir_actions", "files", "操作", "目录", &["复制", "移动", "重命名", "copy", "move", "rename"]),
        // 预览
        item("preview_open", "preview", "打开", "预览", &["open", "按需", "展开"]),
        item("preview_width", "preview", "宽度", "预览", &["width", "分隔线", "拖动"]),
        item("preview_doc", "preview", "文档", "预览", &["pdf", "office", "翻页"]),
        item("preview_theme", "preview", "主题", "预览", &["theme", "外壳", "跟随"]),
        // 快捷键
        item("shortcut_search", "shortcuts", "Ctrl+F", "快捷键", &["搜索", "查找", "find"]),
        item("shortcut_addr", "shortcuts", "Ctrl+L", "快捷键", &["地址", "路径", "location"]),
        item("shortcut_back", "shortcuts", "Alt+左 / 右", "快捷键", &["后退", "前进", "back", "forward"]),
        item("shortcut_up", "shortcuts", "Alt+上", "快捷键", &["上一级", "parent"]),
        item("shortcut_refresh", "shortcuts", "F5", "快捷键", &["刷新", "refresh"]),
        item("shortcut_move", "shortcuts", "方向键", "快捷键", &["移动", "选择", "enter"]),
        item("shortcut_locate", "shortcuts", "Ctrl+Enter", "快捷键", &["定位", "文件", "locate"]),
        item("shortcut_settings", "shortcuts", "Ctrl+,", "快捷键", &["设置", "settings"]),
        // 索引与服务
        item("index_drives", "index", "索引范围", "索引范围", &["drive", "磁盘", "扫描", "勾选", "范围"]),
        item("indexed_locations", "index", "已索引位置", "已索引位置", &["location", "位置", "盘符", "状态"]),
        item("excluded_dirs", "index", "排除目录", "排除目录", &["exclude", "路径", "排除"]),
        item("metadata_backfill", "index", "回填文件的时间和大小", "元数据", &["metadata", "时间", "大小", "回填"]),
        item("full_metadata", "index", "首次建库时读取完整信息", "元数据", &["metadata", "首次", "建库", "完整"]),
        item("everything_ipc", "index", "兼容 Everything 搜索", "元数据", &["everything", "ipc", "兼容"]),
        item("save_interval", "index", "落盘间隔（秒）", "元数据", &["save", "interval", "落盘", "wal", "秒"]),
        item("service_status", "index", "索引服务", "索引服务", &["service", "服务", "启动", "停止", "刷新"]),
        item("service_boot", "index", "服务开机自启", "索引服务", &["autostart", "开机", "自启", "boot"]),
        item("service_login", "index", "登录时自动启动 FindX", "索引服务", &["autostart", "登录", "启动"]),
        item("maintenance", "index", "高级维护", "高级维护", &["maintenance", "压缩", "重建", "备份", "compact", "rebuild"]),
        // 诊断
        item("diag_runtime", "diagnostics", "运行状态", "运行状态", &["status", "版本", "连接", "错误"]),
        item("diag_overlay", "diagnostics", "在主窗口显示诊断浮层", "运行状态", &["overlay", "浮层", "诊断"]),
        item("copy_diag", "diagnostics", "复制诊断", "运行状态", &["copy", "复制", "脱敏", "诊断"]),
        item("diag_storage", "diagnostics", "连接与存储", "连接与存储", &["storage", "索引文件", "管道名称", "服务程序"]),
        item("diag_raw", "diagnostics", "原始状态", "原始状态", &["raw", "状态", "json"]),
        // 关于
        item("about_version", "about", "客户端版本", "关于", &["version", "版本"]),
        item("about_update", "about", "更新", "关于", &["update", "升级", "检查"]),
    ]
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
    fn catalog_covers_content_and_is_unique() {
        let all = all_settings_items();
        assert!(all.len() >= 30, "catalog must cover real settings, got {}", all.len());
        let mut ids = std::collections::HashSet::new();
        for it in &all {
            assert!(ids.insert(it.id), "duplicate settings id: {}", it.id);
            assert!(!it.label.is_empty(), "empty label for {}", it.id);
        }
        for tab in ["appearance", "search", "files", "preview", "index", "shortcuts", "diagnostics", "about"] {
            assert!(all.iter().any(|it| it.tab == tab), "no settings catalog for tab {tab}");
        }
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn pinyin_full_and_initials_match_real_settings() {
        let all = all_settings_items();
        let full = filter(&all, "waiguan");
        assert!(full.iter().any(|it| it.id == "theme_style"), "全拼 waiguan 应命中「主题风格」");
        let initials = filter(&all, "ztfg");
        assert!(initials.iter().any(|it| it.id == "theme_style"), "简拼 ztfg 应命中「主题风格」");
        let sousuo = filter(&all, "sousuo");
        assert!(sousuo.iter().any(|it| it.tab == "search"), "全拼 sousuo 应命中搜索页设置");
        let copy = filter(&all, "fuzhi");
        assert!(copy.iter().any(|it| it.id == "copy_diag"), "全拼 fuzhi 应命中「复制诊断」");
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn pinyin_and_chinese_query_can_be_combined() {
        let all = all_settings_items();
        let out = filter(&all, "pinyin 拼音");
        assert!(out.iter().any(|it| it.id == "pinyin_default"));
        let out = filter(&all, "suoyin fanwei");
        assert!(out.iter().any(|it| it.id == "index_drives"), "全拼 suoyin fanwei 应命中「索引范围」");
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
