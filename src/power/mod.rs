//! What keeps the computer working with its lid closed, and the readings that
//! decide when it has to stop. Each platform provides `read`, `hold` (a `Hold`
//! that lets the computer sleep again when dropped) and `recover`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::{hold, read, recover, Hold};
#[cfg(target_os = "macos")]
pub use macos::{hold, read, recover, Hold};
#[cfg(windows)]
pub use windows::{hold, read, recover, Hold};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub on_battery: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Readings {
    /// None without an internal battery.
    pub battery: Option<Battery>,
    /// None without a lid, or where the system doesn't say.
    pub lid_closed: Option<bool>,
    pub overheating: bool,
}

/// A command's output, or None when it can't run or fails.
#[cfg(not(target_os = "linux"))]
fn output(program: &str, args: &[&str]) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let out = command.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}
