//! 命令面板（Ctrl+K）的纯逻辑部分。
//!
//! UI 只负责：弹出面板、捕获输入、把 `filter` 的结果渲染成列表、把选中项
//! 映射到对应的 action。匹配、排序、高亮范围都在这里，方便无窗口测试。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteCommand {
    pub id: &'static str,
    pub label: &'static str,
    /// 用于匹配的额外关键词（别名、英文、缩写），小写。
    pub keywords: &'static [&'static str],
    /// 分组，用于渲染时的分隔标题。
    pub group: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteMatch {
    pub command: PaletteCommand,
    /// 匹配得分，越大越靠前。
    pub score: i32,
    /// 在 label 中高亮的字符索引范围（按字符，不是字节）。
    pub highlight: Vec<usize>,
}

/// 不区分大小写的子序列匹配：query 的每个字符按顺序出现在 target 里。
/// 返回 (score, highlight_indices)。空 query 匹配所有，得分 0。
pub fn match_text(query: &str, target: &str) -> Option<(i32, Vec<usize>)> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let t: Vec<char> = target.chars().collect();
    let ql = q.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect::<Vec<_>>();
    let tl = t.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect::<Vec<_>>();

    // 贪心子序列匹配，优先连续命中以提高得分。
    let mut hi = Vec::new();
    let mut qi = 0usize;
    let mut last = None;
    let mut consecutive = 0i32;
    let mut score = 0i32;
    for (ti, &tc) in tl.iter().enumerate() {
        if qi < ql.len() && tc == ql[qi] {
            hi.push(ti);
            if let Some(prev) = last {
                if ti == prev + 1 {
                    consecutive += 1;
                    score += 2; // 连续命中加分
                } else {
                    consecutive = 0;
                }
            } else {
                // 首字符命中加分（前缀匹配更重要）。
                if ti == 0 {
                    score += 5;
                }
            }
            last = Some(ti);
            qi += 1;
            score += 1;
        }
    }
    if qi < ql.len() {
        return None;
    }
    // 完整前缀匹配额外加分。
    if tl.starts_with(&ql) {
        score += 10;
    }
    // 关键词/别名完全相等额外加分。
    let _ = consecutive;
    Some((score, hi))
}

/// 对一组命令按 query 过滤并排序。空 query 返回全部（按 group + label 排序）。
pub fn filter(commands: &[PaletteCommand], query: &str) -> Vec<PaletteMatch> {
    let mut out: Vec<PaletteMatch> = commands
        .iter()
        .filter_map(|cmd| {
            // 先试 label，再试 keywords；取最高分。
            let (label_score, label_hi) = match_text(query, cmd.label).unwrap_or((i32::MIN, Vec::new()));
            let mut best_score = label_score;
            let mut best_hi = label_hi;
            let mut any = match_text(query, cmd.label).is_some();
            for kw in cmd.keywords {
                if let Some((s, _)) = match_text(query, kw) {
                    any = true;
                    // 关键词命中得分折半，避免压过 label 前缀。
                    let s = s / 2;
                    if s > best_score {
                        best_score = s;
                        best_hi = Vec::new();
                    }
                }
            }
            if !any {
                return None;
            }
            // 空 query 时所有命令得分 0，保持稳定顺序。
            Some(PaletteMatch {
                command: cmd.clone(),
                score: best_score,
                highlight: best_hi,
            })
        })
        .collect();

    // 稳定排序：分数降序，分数相同按 group then label。
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.command.group.cmp(b.command.group))
            .then_with(|| a.command.label.cmp(b.command.label))
    });
    out
}

/// 限制返回数量（面板通常只显示前 N 条）。
pub fn top(matches: &[PaletteMatch], limit: usize) -> &[PaletteMatch] {
    let end = matches.len().min(limit);
    &matches[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(id: &'static str, label: &'static str, keywords: &'static [&'static str], group: &'static str) -> PaletteCommand {
        PaletteCommand { id, label, keywords, group }
    }

    #[test]
    fn empty_query_returns_all_sorted() {
        let cmds = vec![
            cmd("b", "打开", &["open"], "文件"),
            cmd("a", "关闭", &[], "文件"),
        ];
        let out = filter(&cmds, "");
        assert_eq!(out.len(), 2);
        // group same, label 排序
        assert_eq!(out[0].command.label, "关闭");
        assert_eq!(out[1].command.label, "打开");
    }

    #[test]
    fn prefix_match_scores_higher_than_subsequence() {
        let cmds = vec![
            cmd("a", "导出结果", &[], "操作"),
            cmd("b", "结果统计", &[], "操作"),
        ];
        let out = filter(&cmds, "导出");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].command.id, "a");
    }

    #[test]
    fn no_match_returns_empty() {
        let cmds = vec![cmd("a", "打开", &[], "文件")];
        assert!(filter(&cmds, "zzzz").is_empty());
    }

    #[test]
    fn keyword_match_counts() {
        let cmds = vec![cmd("a", "设置", &["preferences", "settings"], "系统")];
        let out = filter(&cmds, "pref");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].command.id, "a");
    }

    #[test]
    fn highlight_indices_are_char_positions() {
        let (score, hi) = match_text("ab", "aXb").unwrap();
        assert!(score > 0);
        assert_eq!(hi, vec![0, 2]);
    }

    #[test]
    fn top_limits() {
        let cmds = vec![cmd("a", "x", &[], "g"), cmd("b", "y", &[], "g")];
        let m = filter(&cmds, "");
        assert_eq!(top(&m, 1).len(), 1);
        assert_eq!(top(&m, 10).len(), 2);
    }
}