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
    let msg = format!(
        "\x1b[1mWelcome to omnish!\x1b[0m\n\
         \n\
         \x1b[36m  :  <query>\x1b[0m    Chat with AI about your terminal activity\n\
         \x1b[36m  :: <query>\x1b[0m    Resume your last conversation\n\
         \x1b[36m  Tab\x1b[0m           Accept ghost completion suggestion\n\
         \n\
         \x1b[2m  Config: {}\x1b[0m\n",
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
