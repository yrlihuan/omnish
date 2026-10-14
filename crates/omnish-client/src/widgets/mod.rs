pub mod chat_layout;
pub mod common;
pub mod inline_notice;
pub mod line_editor;
pub mod line_prompt;
pub mod line_status;
pub mod menu;
pub mod menu_notice;
pub mod picker;
pub mod scroll_view;
pub mod text_view;

/// A menu-open guard for tests outside the widgets module.
#[cfg(test)]
pub fn menu_notice_test_guard() -> impl Drop {
    menu_notice::MenuOpenGuard::new()
}
