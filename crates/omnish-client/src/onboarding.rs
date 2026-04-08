use std::path::PathBuf;

/// Return the resolved client.toml path.
fn client_toml_path() -> PathBuf {
    std::env::var("OMNISH_CLIENT_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| omnish_common::config::omnish_dir().join("client.toml"))
}

/// Print the welcome message to stdout (before shell prompt appears).
pub fn print_welcome() {
    let config_path = client_toml_path();
    let config_display = config_path.display();
    use crate::display::{BOLD, CYAN, DIM, RESET};
    let msg = format!(
        "{BOLD}Welcome to omnish!{RESET}\n\
         \n\
         {CYAN}  :  <query>{RESET}    Chat with AI about your terminal activity\n\
         {CYAN}  :: <query>{RESET}    Resume your last conversation\n\
         {CYAN}  Tab{RESET}           Accept ghost completion suggestion\n\
         \n\
         {DIM}  Config: {}{RESET}\n",
        config_display,
    );
    print!("{}", msg);
}

/// Write `onboarded = true` to client.toml, preserving existing formatting.
pub fn mark_onboarded() {
    let path = client_toml_path();
    if let Err(e) = omnish_common::config_edit::set_toml_value(&path, "onboarded", true) {
        tracing::warn!("cannot write onboarded flag to client.toml: {}", e);
    }
}
