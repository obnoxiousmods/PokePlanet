//! Opening the player's web browser for the Discord login.

/// Launch `url` in the default browser.
///
/// Only http(s) URLs are accepted. The URL originates from the server, but a compromised
/// or misconfigured server should not be able to hand us a `file:` or shell-interpreted
/// target, so it is validated before it reaches the OS.
pub fn open(url: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        url.starts_with("https://") || url.starts_with("http://"),
        "refusing to open a non-http URL"
    );
    anyhow::ensure!(
        !url.contains(['"', '\n', '\r']),
        "refusing to open a URL containing quotes or newlines"
    );
    open_platform(url)
}

#[cfg(windows)]
fn open_platform(url: &str) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: without it, cmd.exe flashes a console over the game.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // `start` treats its first quoted argument as a window title, hence the empty "".
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

#[cfg(not(windows))]
fn open_platform(url: &str) -> anyhow::Result<()> {
    std::process::Command::new("xdg-open").arg(url).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::open;

    #[test]
    fn non_http_schemes_are_refused() {
        assert!(open("file:///etc/passwd").is_err());
        assert!(open("javascript:alert(1)").is_err());
    }

    #[test]
    fn quoting_tricks_are_refused() {
        assert!(open("https://example.com/\" & calc.exe &\"").is_err());
    }
}
