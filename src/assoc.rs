use std::path::Path;

use anyhow::Result;

#[cfg(windows)]
pub fn associate(exe: &Path, mid: bool) -> Result<()> {
    use anyhow::Context;
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu
        .open_subkey_with_flags("Software\\Classes", KEY_WRITE)
        .context("opening HKCU\\Software\\Classes")?;

    let exe = exe.display().to_string();
    let command = format!("\"{exe}\" gui \"%1\"");

    let mut entries = vec![(".kar", "DontreeCrab.kar", "DontreeCrab karaoke song")];
    if mid {
        entries.push((".mid", "DontreeCrab.mid", "DontreeCrab MIDI song"));
        entries.push((".midi", "DontreeCrab.mid", "DontreeCrab MIDI song"));
    }

    for (extension, prog_id, description) in entries {
        let (ext_key, _) = classes
            .create_subkey(extension)
            .with_context(|| format!("creating {extension}"))?;
        ext_key.set_value("", &prog_id)?;
        let (open_with, _) = ext_key.create_subkey("OpenWithProgids")?;
        open_with.set_value(prog_id, &"")?;

        let (prog, _) = classes.create_subkey(prog_id)?;
        prog.set_value("", &description)?;

        let (icon, _) = prog.create_subkey("DefaultIcon")?;
        icon.set_value("", &format!("\"{exe}\",0"))?;

        let (cmd, _) = prog.create_subkey("shell\\open\\command")?;
        cmd.set_value("", &command)?;
    }

    notify_shell();
    Ok(())
}

#[cfg(windows)]
pub fn unassociate() -> Result<()> {
    use anyhow::Context;
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu
        .open_subkey_with_flags("Software\\Classes", KEY_READ | KEY_WRITE)
        .context("opening HKCU\\Software\\Classes")?;

    // Only remove an extension if it currently points at us (new or legacy
    // program ids are both cleaned up).
    for (extension, prog_ids) in [
        (".kar", ["DontreeCrab.kar", "KaraokePlayer.kar"]),
        (".mid", ["DontreeCrab.mid", "KaraokePlayer.mid"]),
        (".midi", ["DontreeCrab.mid", "KaraokePlayer.mid"]),
    ] {
        if let Ok(key) = classes.open_subkey(extension) {
            let current: String = key.get_value("").unwrap_or_default();
            if prog_ids.contains(&current.as_str()) {
                let _ = classes.delete_subkey_all(extension);
            }
        }
    }

    for class in [
        "DontreeCrab.kar",
        "DontreeCrab.mid",
        "KaraokePlayer.kar",
        "KaraokePlayer.mid",
    ] {
        let _ = classes.delete_subkey_all(class);
    }

    notify_shell();
    Ok(())
}

#[cfg(windows)]
fn notify_shell() {
    use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

    // Ask Explorer to forget its cached association table.
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED as i32,
            SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        );
    }
}

#[cfg(not(windows))]
pub fn associate(_exe: &Path, _mid: bool) -> Result<()> {
    anyhow::bail!("file associations are only implemented on Windows")
}

#[cfg(not(windows))]
pub fn unassociate() -> Result<()> {
    anyhow::bail!("file associations are only implemented on Windows")
}
