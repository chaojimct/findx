//! Rebindable keyboard shortcuts.
//!
//! GPUI's keymap is additive: bindings added later win, and an `Unbind` marker
//! suppresses an earlier binding for the same keystroke. We keep all of the
//! built-in (component / startup) bindings untouched and only ever *add*
//! bindings plus matching `Unbind` markers when the user edits a shortcut.
//! That avoids `clear_key_bindings()`, which would also throw away bindings
//! that components register lazily when a menu or popup first opens.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use gpui_kit::{
    Action, App, KeyBinding, KeyBindingContextPredicate, Keystroke, SharedString, Unbind,
};

use crate::ui::main_window::*;

pub const FILE_CONTEXT: &str = "FindXMain && !Input";
pub const MAIN_CONTEXT: &str = "FindXMain";

pub struct KeyDef {
    pub id: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    /// All factory bindings for this action. Aliases are listed together so a
    /// user override replaces every one of them.
    pub defaults: &'static [&'static str],
    pub context: &'static str,
    /// Fully-qualified GPUI action name, used by `Unbind`.
    pub action_name: &'static str,
}

pub fn defs() -> &'static [KeyDef] {
    &[
        KeyDef { id: "focus_search", label: "聚焦搜索框", group: "导航", defaults: &["ctrl-f"], context: MAIN_CONTEXT, action_name: "findx_native::FocusSearch" },
        KeyDef { id: "open_settings", label: "打开设置", group: "导航", defaults: &["ctrl-comma"], context: MAIN_CONTEXT, action_name: "findx_native::OpenSettings" },
        KeyDef { id: "toggle_command_palette", label: "命令面板", group: "导航", defaults: &["ctrl-k", "f1", "ctrl-shift-p"], context: MAIN_CONTEXT, action_name: "findx_native::ToggleCommandPalette" },
        KeyDef { id: "workspace_back", label: "后退", group: "导航", defaults: &["alt-left"], context: FILE_CONTEXT, action_name: "findx_native::WorkspaceBack" },
        KeyDef { id: "workspace_forward", label: "前进", group: "导航", defaults: &["alt-right"], context: FILE_CONTEXT, action_name: "findx_native::WorkspaceForward" },
        KeyDef { id: "workspace_up", label: "上一级目录", group: "导航", defaults: &["alt-up"], context: FILE_CONTEXT, action_name: "findx_native::WorkspaceUp" },
        KeyDef { id: "workspace_focus_path", label: "编辑地址栏", group: "导航", defaults: &["ctrl-l", "alt-d"], context: MAIN_CONTEXT, action_name: "findx_native::WorkspaceFocusPath" },
        KeyDef { id: "parent_selected", label: "返回上层目录", group: "导航", defaults: &["backspace", "ctrl-backspace"], context: FILE_CONTEXT, action_name: "findx_native::ParentSelected" },
        KeyDef { id: "toggle_diag", label: "诊断浮层", group: "视图", defaults: &["ctrl-shift-d"], context: FILE_CONTEXT, action_name: "findx_native::ToggleDiag" },
        KeyDef { id: "toggle_sidebar", label: "显示/隐藏侧栏", group: "视图", defaults: &["ctrl-b"], context: FILE_CONTEXT, action_name: "findx_native::ToggleSidebar" },
        KeyDef { id: "preview_selected", label: "显示/隐藏预览", group: "视图", defaults: &["ctrl-i"], context: FILE_CONTEXT, action_name: "findx_native::PreviewSelected" },
        KeyDef { id: "directory_new_tab", label: "新建目录标签", group: "标签", defaults: &["ctrl-t", "ctrl-n"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryNewTab" },
        KeyDef { id: "directory_close_tab", label: "关闭当前标签", group: "标签", defaults: &["ctrl-w"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryCloseTab" },
        KeyDef { id: "directory_next_tab", label: "下一个标签", group: "标签", defaults: &["ctrl-tab"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryNextTab" },
        KeyDef { id: "directory_previous_tab", label: "上一个标签", group: "标签", defaults: &["ctrl-shift-tab"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryPreviousTab" },
        KeyDef { id: "directory_last_tab", label: "最后一个标签", group: "标签", defaults: &["ctrl-0"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryLastTab" },
        KeyDef { id: "directory_tab_1", label: "切换到标签 1", group: "标签", defaults: &["ctrl-1"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryTab1" },
        KeyDef { id: "directory_tab_2", label: "切换到标签 2", group: "标签", defaults: &["ctrl-2"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryTab2" },
        KeyDef { id: "directory_tab_3", label: "切换到标签 3", group: "标签", defaults: &["ctrl-3"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryTab3" },
        KeyDef { id: "directory_tab_4", label: "切换到标签 4", group: "标签", defaults: &["ctrl-4"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryTab4" },
        KeyDef { id: "directory_tab_5", label: "切换到标签 5", group: "标签", defaults: &["ctrl-5"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryTab5" },
        KeyDef { id: "directory_tab_6", label: "切换到标签 6", group: "标签", defaults: &["ctrl-6"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryTab6" },
        KeyDef { id: "directory_tab_7", label: "切换到标签 7", group: "标签", defaults: &["ctrl-7"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryTab7" },
        KeyDef { id: "directory_tab_8", label: "切换到标签 8", group: "标签", defaults: &["ctrl-8"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryTab8" },
        KeyDef { id: "directory_tab_9", label: "切换到标签 9", group: "标签", defaults: &["ctrl-9"], context: FILE_CONTEXT, action_name: "findx_native::DirectoryTab9" },
        KeyDef { id: "select_next_result", label: "下一项", group: "选择", defaults: &["down"], context: FILE_CONTEXT, action_name: "findx_native::SelectNextResult" },
        KeyDef { id: "select_prev_result", label: "上一项", group: "选择", defaults: &["up"], context: FILE_CONTEXT, action_name: "findx_native::SelectPrevResult" },
        KeyDef { id: "select_first_result", label: "第一项", group: "选择", defaults: &["home"], context: FILE_CONTEXT, action_name: "findx_native::SelectFirstResult" },
        KeyDef { id: "select_last_result", label: "最后一项", group: "选择", defaults: &["end"], context: FILE_CONTEXT, action_name: "findx_native::SelectLastResult" },
        KeyDef { id: "reveal_selected", label: "在目录中定位", group: "选择", defaults: &["ctrl-enter"], context: FILE_CONTEXT, action_name: "findx_native::RevealSelected" },
        KeyDef { id: "context_select_all", label: "全选", group: "选择", defaults: &["ctrl-a"], context: FILE_CONTEXT, action_name: "findx_native::ContextSelectAll" },
        KeyDef { id: "workspace_refresh", label: "刷新", group: "选择", defaults: &["f5", "ctrl-r"], context: FILE_CONTEXT, action_name: "findx_native::WorkspaceRefresh" },
        KeyDef { id: "context_copy", label: "复制文件", group: "文件", defaults: &["ctrl-c"], context: FILE_CONTEXT, action_name: "findx_native::ContextCopy" },
        KeyDef { id: "context_cut", label: "剪切文件", group: "文件", defaults: &["ctrl-x"], context: FILE_CONTEXT, action_name: "findx_native::ContextCut" },
        KeyDef { id: "context_paste", label: "粘贴文件", group: "文件", defaults: &["ctrl-v"], context: FILE_CONTEXT, action_name: "findx_native::ContextPaste" },
        KeyDef { id: "context_copy_path", label: "复制路径", group: "文件", defaults: &["ctrl-shift-c"], context: FILE_CONTEXT, action_name: "findx_native::ContextCopyPath" },
        KeyDef { id: "context_rename", label: "重命名", group: "文件", defaults: &["f2"], context: FILE_CONTEXT, action_name: "findx_native::ContextRename" },
        KeyDef { id: "context_properties", label: "属性", group: "文件", defaults: &["alt-enter"], context: FILE_CONTEXT, action_name: "findx_native::ContextProperties" },
        KeyDef { id: "show_file_menu", label: "文件菜单", group: "文件", defaults: &["shift-f10", "menu"], context: FILE_CONTEXT, action_name: "findx_native::ShowFileMenu" },
    ]
}

pub fn def(id: &str) -> Option<&'static KeyDef> {
    defs().iter().find(|d| d.id == id)
}

pub fn default_for(id: &str) -> &'static str {
    def(id).and_then(|d| d.defaults.first().copied()).unwrap_or("")
}

fn action_box(id: &str) -> Option<Box<dyn Action>> {
    let action: Box<dyn Action> = match id {
        "focus_search" => Box::new(FocusSearch),
        "open_settings" => Box::new(OpenSettings),
        "toggle_command_palette" => Box::new(ToggleCommandPalette),
        "workspace_back" => Box::new(WorkspaceBack),
        "workspace_forward" => Box::new(WorkspaceForward),
        "workspace_up" => Box::new(WorkspaceUp),
        "workspace_focus_path" => Box::new(WorkspaceFocusPath),
        "parent_selected" => Box::new(ParentSelected),
        "toggle_diag" => Box::new(ToggleDiag),
        "toggle_sidebar" => Box::new(ToggleSidebar),
        "preview_selected" => Box::new(PreviewSelected),
        "directory_new_tab" => Box::new(DirectoryNewTab),
        "directory_close_tab" => Box::new(DirectoryCloseTab),
        "directory_next_tab" => Box::new(DirectoryNextTab),
        "directory_previous_tab" => Box::new(DirectoryPreviousTab),
        "directory_last_tab" => Box::new(DirectoryLastTab),
        "directory_tab_1" => Box::new(DirectoryTab1),
        "directory_tab_2" => Box::new(DirectoryTab2),
        "directory_tab_3" => Box::new(DirectoryTab3),
        "directory_tab_4" => Box::new(DirectoryTab4),
        "directory_tab_5" => Box::new(DirectoryTab5),
        "directory_tab_6" => Box::new(DirectoryTab6),
        "directory_tab_7" => Box::new(DirectoryTab7),
        "directory_tab_8" => Box::new(DirectoryTab8),
        "directory_tab_9" => Box::new(DirectoryTab9),
        "select_next_result" => Box::new(SelectNextResult),
        "select_prev_result" => Box::new(SelectPrevResult),
        "select_first_result" => Box::new(SelectFirstResult),
        "select_last_result" => Box::new(SelectLastResult),
        "reveal_selected" => Box::new(RevealSelected),
        "context_select_all" => Box::new(ContextSelectAll),
        "workspace_refresh" => Box::new(WorkspaceRefresh),
        "context_copy" => Box::new(ContextCopy),
        "context_cut" => Box::new(ContextCut),
        "context_paste" => Box::new(ContextPaste),
        "context_copy_path" => Box::new(ContextCopyPath),
        "context_rename" => Box::new(ContextRename),
        "context_properties" => Box::new(ContextProperties),
        "show_file_menu" => Box::new(ShowFileMenu),
        _ => return None,
    };
    Some(action)
}

// Shortcuts currently overridden in this process (action id -> keystroke).
thread_local! {
    static APPLIED: RefCell<BTreeMap<String, String>> = RefCell::new(BTreeMap::new());
}

pub fn applied_overrides() -> BTreeMap<String, String> {
    APPLIED.with(|a| a.borrow().clone())
}

fn context_predicate(context: &str) -> Option<Rc<KeyBindingContextPredicate>> {
    KeyBindingContextPredicate::parse(context).ok().map(Rc::new)
}

fn bind(stroke: &str, definition: &KeyDef, cx: &mut App) -> bool {
    let Some(action) = action_box(definition.id) else {
        return false;
    };
    match KeyBinding::load(
        stroke,
        action,
        context_predicate(definition.context),
        false,
        None,
        cx,
    ) {
        Ok(binding) => {
            cx.bind_keys([binding]);
            true
        }
        Err(_) => false,
    }
}

fn valid_stroke(stroke: &str) -> bool {
    let parts: Vec<&str> = stroke.split('-').collect();
    let Some(key) = parts.last() else {
        return false;
    };
    if key.is_empty() {
        return false;
    }
    parts[..parts.len() - 1].iter().all(|part| {
        matches!(
            *part,
            "ctrl" | "alt" | "shift" | "win" | "fn" | "cmd" | "super" | "meta" | "function"
        )
    })
}

fn unbind(stroke: &str, definition: &KeyDef, cx: &mut App) {
    if !valid_stroke(stroke) {
        return;
    }
    let marker = Unbind(SharedString::from(definition.action_name));
    if let Ok(binding) = KeyBinding::load(stroke, Box::new(marker), None, false, None, cx) {
        cx.bind_keys([binding]);
    }
}

/// Reconcile the live GPUI keymap with the saved overrides. Safe to call any
/// number of times: old overrides are explicitly unbound before new ones are
/// installed, so no duplicate actions accumulate.
pub fn apply(overrides: &BTreeMap<String, String>, cx: &mut App) {
    let previous = APPLIED.with(|applied| std::mem::take(&mut *applied.borrow_mut()));
    for definition in defs() {
        let old = previous.get(definition.id).map(String::as_str).unwrap_or("");
        let new = overrides.get(definition.id).map(String::as_str).unwrap_or("");
        if old.is_empty() || old == new {
            continue;
        }
        unbind(old, definition, cx);
        for default in definition.defaults {
            bind(default, definition, cx);
        }
    }
    for definition in defs() {
        let old = previous.get(definition.id).map(String::as_str).unwrap_or("");
        let new = overrides.get(definition.id).map(String::as_str).unwrap_or("");
        if new.is_empty() || new == old || definition.defaults.contains(&new) {
            continue;
        }
        for default in definition.defaults {
            unbind(default, definition, cx);
        }
        bind(new, definition, cx);
    }
    let next = overrides
        .iter()
        .filter(|(id, stroke)| {
            def(id).is_some_and(|d| !stroke.is_empty() && !d.defaults.contains(&stroke.as_str()))
        })
        .map(|(id, stroke)| (id.clone(), stroke.clone()))
        .collect();
    APPLIED.with(|applied| *applied.borrow_mut() = next);
}

/// Normalise a captured key event into the canonical `ctrl-shift-c` form used
/// by GPUI bindings. Bare modifiers and unparsable keys are rejected.
pub fn from_keystroke(key: &Keystroke) -> Option<String> {
    let base = key.key.to_ascii_lowercase();
    if base.is_empty()
        || matches!(
            base.as_str(),
            "ctrl" | "control" | "alt" | "shift" | "meta" | "cmd" | "win" | "super" | "fn"
        )
    {
        return None;
    }
    let mut parts = Vec::new();
    if key.modifiers.control {
        parts.push("ctrl");
    }
    if key.modifiers.alt {
        parts.push("alt");
    }
    if key.modifiers.shift {
        parts.push("shift");
    }
    if key.modifiers.platform {
        parts.push("win");
    }
    if key.modifiers.function {
        parts.push("fn");
    }
    parts.push(base.as_str());
    let stroke = parts.join("-");
    valid_stroke(&stroke).then_some(stroke)
}

/// Human-readable form for the settings list.
pub fn display(stroke: &str) -> String {
    stroke
        .split('-')
        .map(|part| match part {
            "ctrl" => "Ctrl".to_string(),
            "alt" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "win" => "Win".to_string(),
            "fn" => "Fn".to_string(),
            "comma" => ",".to_string(),
            "tab" => "Tab".to_string(),
            "enter" => "Enter".to_string(),
            "backspace" => "Backspace".to_string(),
            "space" => "Space".to_string(),
            "menu" => "Menu".to_string(),
            other if other.starts_with('f') && other[1..].parse::<u8>().is_ok() => {
                other.to_ascii_uppercase()
            }
            other if other.chars().count() == 1 => other.to_ascii_uppercase(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Effective shortcut for an action, honouring saved overrides.
pub fn effective(id: &str, overrides: &BTreeMap<String, String>) -> String {
    overrides
        .get(id)
        .filter(|stroke| !stroke.is_empty())
        .cloned()
        .unwrap_or_else(|| default_for(id).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_definition_has_a_live_action_and_parseable_defaults() {
        for definition in defs() {
            assert!(action_box(definition.id).is_some(), "{}", definition.id);
            assert!(!definition.defaults.is_empty(), "{}", definition.id);
            for default in definition.defaults {
                assert!(Keystroke::parse(default).is_ok(), "{default}");
            }
            assert!(
                definition.action_name.starts_with("findx_native::"),
                "{}",
                definition.id
            );
            assert_eq!(
                action_box(definition.id).unwrap().name(),
                definition.action_name,
                "registry action name must match the real GPUI action"
            );
        }
    }

    #[test]
    fn every_registered_default_is_a_factory_binding() {
        use gpui_kit::{KeyContext, Keymap};
        let map = Keymap::new(crate::ui::main_window::main_key_bindings());
        let stack = [KeyContext::parse("FindXMain").unwrap()];
        for definition in defs() {
            for default in definition.defaults {
                let stroke = Keystroke::parse(default).unwrap();
                let (bindings, _) = map.bindings_for_input(&[stroke], &stack);
                assert!(
                    bindings
                        .iter()
                        .any(|b| b.action().name() == definition.action_name),
                    "{} is missing factory binding {default}",
                    definition.id
                );
            }
        }
    }

    #[test]
    fn definition_ids_are_unique() {
        let mut ids = std::collections::BTreeSet::new();
        for definition in defs() {
            assert!(ids.insert(definition.id), "duplicate {}", definition.id);
        }
    }

    #[test]
    fn captured_keystrokes_use_canonical_binding_form() {
        let mut k = Keystroke::parse("ctrl-shift-c").unwrap();
        k.key = "C".into();
        assert_eq!(from_keystroke(&k).as_deref(), Some("ctrl-shift-c"));
        assert_eq!(display("ctrl-shift-c"), "Ctrl+Shift+C");
        assert_eq!(display("ctrl-comma"), "Ctrl+,");
        assert_eq!(display("f1"), "F1");
        let bare = Keystroke::parse("shift").unwrap();
        assert_eq!(from_keystroke(&bare), None);
    }

    #[test]
    fn effective_honours_override_but_ignores_empty() {
        let mut map = BTreeMap::new();
        map.insert("focus_search".to_string(), "ctrl-g".to_string());
        map.insert("open_settings".to_string(), String::new());
        assert_eq!(effective("focus_search", &map), "ctrl-g");
        assert_eq!(effective("open_settings", &map), "ctrl-comma");
    }
}
