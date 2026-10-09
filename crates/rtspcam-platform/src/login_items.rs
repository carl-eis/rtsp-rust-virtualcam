//! Autostart entries that are plain files: an XDG autostart `.desktop` file (Linux) and a
//! LaunchAgent property list (macOS).
//!
//! Writing them is ordinary file I/O, so this module is compiled (and tested) on every OS; the
//! Linux and macOS modules pick the one their desktop reads.

// Each OS uses one of the two; both are compiled and tested everywhere.
#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::autostart::{Autostart, MINIMIZED_ARG};

/// `rtspcam.desktop` in an XDG autostart folder (`$XDG_CONFIG_HOME/autostart`), which desktop
/// sessions start at login.
#[derive(Debug, Clone)]
pub(crate) struct XdgAutostart {
    file: PathBuf,
}

impl XdgAutostart {
    /// The entry in `dir`, normally `~/.config/autostart`.
    pub(crate) fn in_dir(dir: &Path) -> Self {
        Self {
            file: dir.join("rtspcam.desktop"),
        }
    }

    fn contents(exe: &Path) -> String {
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=RTSP Cam\n\
             Comment=Turns RTSP streams into webcams\n\
             Exec={} {MINIMIZED_ARG}\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n",
            desktop_exec_quote(&exe.to_string_lossy())
        )
    }
}

impl Autostart for XdgAutostart {
    fn label(&self) -> &'static str {
        "Start at login"
    }

    fn is_enabled(&self) -> bool {
        self.file.exists()
    }

    fn set(&self, enabled: bool) -> io::Result<()> {
        if enabled {
            write_entry(&self.file, &Self::contents(&std::env::current_exe()?))
        } else {
            remove_entry(&self.file)
        }
    }
}

/// A LaunchAgent in `~/Library/LaunchAgents`; launchd runs it at login (`RunAtLoad`).
#[derive(Debug, Clone)]
pub(crate) struct LaunchAgent {
    label: String,
    file: PathBuf,
}

impl LaunchAgent {
    /// The agent `label` (a reverse-DNS name) in `dir`, normally `~/Library/LaunchAgents`.
    pub(crate) fn in_dir(dir: &Path, label: &str) -> Self {
        Self {
            label: label.to_owned(),
            file: dir.join(format!("{label}.plist")),
        }
    }

    fn contents(&self, exe: &Path) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \t<key>Label</key>\n\
             \t<string>{}</string>\n\
             \t<key>ProgramArguments</key>\n\
             \t<array>\n\
             \t\t<string>{}</string>\n\
             \t\t<string>{MINIMIZED_ARG}</string>\n\
             \t</array>\n\
             \t<key>RunAtLoad</key>\n\
             \t<true/>\n\
             \t<key>ProcessType</key>\n\
             \t<string>Interactive</string>\n\
             </dict>\n\
             </plist>\n",
            xml_escape(&self.label),
            xml_escape(&exe.to_string_lossy())
        )
    }
}

impl Autostart for LaunchAgent {
    fn label(&self) -> &'static str {
        "Start at login"
    }

    fn is_enabled(&self) -> bool {
        self.file.exists()
    }

    fn set(&self, enabled: bool) -> io::Result<()> {
        if enabled {
            write_entry(&self.file, &self.contents(&std::env::current_exe()?))
        } else {
            remove_entry(&self.file)
        }
    }
}

fn write_entry(file: &Path, contents: &str) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(file, contents)
}

fn remove_entry(file: &Path) -> io::Result<()> {
    match fs::remove_file(file) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Quotes a program path for a desktop entry's `Exec` key: double quotes, with `"`, `` ` ``,
/// `$` and `\` escaped by a backslash, and `%` doubled (it starts a field code).
fn desktop_exec_quote(path: &str) -> String {
    let mut out = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_paths_are_quoted_for_desktop_files() {
        assert_eq!(
            desktop_exec_quote("/opt/RTSP Cam/rtspcam"),
            "\"/opt/RTSP Cam/rtspcam\""
        );
        assert_eq!(desktop_exec_quote(r#"/a$b"c\d%e"#), r#""/a\$b\"c\\d%%e""#);
        let text = XdgAutostart::contents(Path::new("/opt/rtspcam"));
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(
            text.contains("\nExec=\"/opt/rtspcam\" --minimized\n"),
            "{text}"
        );
    }

    #[test]
    fn launch_agents_run_the_app_minimized_at_load() {
        let agent = LaunchAgent::in_dir(Path::new("/x"), "io.github.carl-eis.rtspcam");
        let text = agent.contents(Path::new(
            "/Applications/R&D Cam.app/Contents/MacOS/rtspcam",
        ));
        assert!(text.contains("<string>io.github.carl-eis.rtspcam</string>"));
        assert!(
            text.contains("<string>/Applications/R&amp;D Cam.app/Contents/MacOS/rtspcam</string>")
        );
        assert!(text.contains("<string>--minimized</string>"));
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert_eq!(
            agent.file,
            Path::new("/x").join("io.github.carl-eis.rtspcam.plist")
        );
    }

    #[test]
    fn entries_are_written_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        for entry in [
            Box::new(XdgAutostart::in_dir(&dir.path().join("autostart"))) as Box<dyn Autostart>,
            Box::new(LaunchAgent::in_dir(
                &dir.path().join("LaunchAgents"),
                "a.b.c",
            )),
        ] {
            assert!(!entry.is_enabled());
            entry.set(true).unwrap();
            assert!(entry.is_enabled());
            entry.set(false).unwrap();
            assert!(!entry.is_enabled());
            // Removing twice is fine.
            entry.set(false).unwrap();
        }
    }
}
