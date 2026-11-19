//! "Launch at login": an XDG autostart entry
//! (`$XDG_CONFIG_HOME/autostart/dev.ferro.ferrogram.desktop`) whose `Exec`
//! carries `--minimized`, so the app comes up as a tray icon only. The file
//! *is* the setting: the toggle reflects whether it exists, and turning it
//! off deletes it. Off by default.

use std::io;
use std::path::{Path, PathBuf};

use crate::config::{APP_ID, APP_NAME};

/// `$XDG_CONFIG_HOME` (when absolute, per the spec) or `$HOME/.config`.
/// `env` is injectable so tests never touch the real environment.
pub fn config_home(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(dir) = env("XDG_CONFIG_HOME").map(PathBuf::from)
        && dir.is_absolute()
    {
        return Some(dir);
    }
    env("HOME")
        .map(|h| PathBuf::from(h).join(".config"))
        .filter(|p| p.is_absolute())
}

/// [`config_home`] for the real process environment.
pub fn system_config_home() -> Option<PathBuf> {
    config_home(&|k| std::env::var(k).ok())
}

pub fn entry_path(config_home: &Path) -> PathBuf {
    config_home
        .join("autostart")
        .join(format!("{APP_ID}.desktop"))
}

/// Quote one `Exec` argument (Desktop Entry spec, "The Exec key"): inside
/// double quotes `"`, `` ` ``, `$` and `\` take a backslash, then the string
/// escape rule doubles every backslash; `%` is the field-code introducer.
fn exec_quote(arg: &str) -> String {
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The entry's text, starting `exe` with `--minimized`.
pub fn entry_text(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={APP_NAME}\n\
         Comment=Start {APP_NAME} in the system tray\n\
         Exec={} --minimized\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n\
         X-KDE-autostart-after=panel\n",
        exec_quote(&exe.display().to_string())
    )
}

pub fn is_enabled(config_home: &Path) -> bool {
    entry_path(config_home).is_file()
}

/// Write the entry; returns its path.
pub fn enable(config_home: &Path, exe: &Path) -> io::Result<PathBuf> {
    let path = entry_path(config_home);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, entry_text(exe))?;
    Ok(path)
}

/// Remove the entry; already gone is fine.
pub fn disable(config_home: &Path) -> io::Result<()> {
    match std::fs::remove_file(entry_path(config_home)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Switch autostart on or off for the real user (blocking: touches disk).
pub fn set_enabled(on: bool) -> io::Result<bool> {
    let home = system_config_home()
        .ok_or_else(|| io::Error::other("no XDG_CONFIG_HOME or HOME in the environment"))?;
    if on {
        let exe = std::env::current_exe()?;
        enable(&home, &exe)?;
    } else {
        disable(&home)?;
    }
    Ok(is_enabled(&home))
}

/// Whether autostart is currently on for the real user.
pub fn system_is_enabled() -> bool {
    system_config_home().is_some_and(|h| is_enabled(&h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_home_follows_the_xdg_rules() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            config_home(&env(&[("XDG_CONFIG_HOME", "/x/cfg"), ("HOME", "/home/u")])),
            Some(PathBuf::from("/x/cfg"))
        );
        // A relative XDG_CONFIG_HOME must be ignored.
        assert_eq!(
            config_home(&env(&[("XDG_CONFIG_HOME", "cfg"), ("HOME", "/home/u")])),
            Some(PathBuf::from("/home/u/.config"))
        );
        assert_eq!(
            config_home(&env(&[("HOME", "/home/u")])),
            Some(PathBuf::from("/home/u/.config"))
        );
        assert_eq!(config_home(&env(&[])), None);
    }

    #[test]
    fn create_and_remove_in_a_temp_xdg_config_home() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_owned();
        let env = move |k: &str| (k == "XDG_CONFIG_HOME").then(|| root.clone());
        let home = config_home(&env).unwrap();
        assert!(!is_enabled(&home), "off by default");

        let exe = Path::new("/opt/Ferro gram/ferrogram");
        let path = enable(&home, exe).unwrap();
        assert_eq!(
            path,
            dir.path().join("autostart/dev.ferro.ferrogram.desktop")
        );
        assert!(is_enabled(&home));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(
            text.contains("Exec=\"/opt/Ferro gram/ferrogram\" --minimized\n"),
            "{text}"
        );
        assert!(text.contains("Icon=dev.ferro.ferrogram"));
        assert!(text.contains("X-KDE-autostart-after=panel"));

        // Enabling twice rewrites in place; disabling removes the file.
        enable(&home, exe).unwrap();
        disable(&home).unwrap();
        assert!(!is_enabled(&home));
        assert!(!path.exists());
        disable(&home).unwrap();
    }

    #[test]
    fn exec_arguments_are_quoted_per_the_spec() {
        assert_eq!(exec_quote("/a b/c"), "\"/a b/c\"");
        assert_eq!(exec_quote("a\"b"), "\"a\\\\\"b\"");
        assert_eq!(exec_quote("$HOME`x`"), "\"\\\\$HOME\\\\`x\\\\`\"");
        assert_eq!(exec_quote("c:\\x"), "\"c:\\\\\\\\x\"");
        assert_eq!(exec_quote("100%"), "\"100%%\"");
    }
}
