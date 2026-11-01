use super::{Battery, Readings};
use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

/// A logind block inhibitor on sleep, idle and the lid switch, held by a
/// `systemd-inhibit` child for as long as this lives.
pub struct Hold(Child);

pub fn hold(_dir: &Path) -> Result<Hold, String> {
    let unavailable = || "Needs systemd-logind to keep running with the lid closed".to_string();
    // tail --pid ends the inhibitor with Aloft, even if Aloft crashes.
    let mut child = Command::new("systemd-inhibit")
        .args([
            "--what=sleep:idle:handle-lid-switch",
            "--who=Flokque Aloft",
            "--why=Keeps working with the lid closed",
            "--mode=block",
            "tail",
            &format!("--pid={}", std::process::id()),
            "-f",
            "/dev/null",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| unavailable())?;
    // systemd-inhibit exits straight away when logind or polkit refuses.
    thread::sleep(Duration::from_millis(300));
    match child.try_wait() {
        Ok(None) => Ok(Hold(child)),
        _ => Err(unavailable()),
    }
}

impl Hold {
    pub fn detail(&self) -> &'static str {
        "Lid-safe"
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn recover(_dir: &Path) {}

pub fn read() -> Readings {
    Readings {
        battery: battery(),
        lid_closed: lid_closed(),
        overheating: overheating(),
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn entries(dir: &str) -> Vec<std::path::PathBuf> {
    fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// The system battery; a mouse or headset reports one too, with scope "Device".
fn battery() -> Option<Battery> {
    entries("/sys/class/power_supply")
        .into_iter()
        .find_map(|dir| {
            let read = |name| read_trimmed(&dir.join(name));
            if read("type")? != "Battery" || read("scope").as_deref() == Some("Device") {
                return None;
            }
            Some(Battery {
                percent: read("capacity")?.parse().ok()?,
                on_battery: read("status")? == "Discharging",
            })
        })
}

fn lid_closed() -> Option<bool> {
    entries("/proc/acpi/button/lid")
        .into_iter()
        .find_map(|dir| parse_lid(&read_trimmed(&dir.join("state"))?))
}

/// `state:      open`
fn parse_lid(state: &str) -> Option<bool> {
    match state.split(':').nth(1)?.trim() {
        "closed" => Some(true),
        "open" => Some(false),
        _ => None,
    }
}

/// Within 5 °C of a thermal zone's critical trip point.
fn overheating() -> bool {
    entries("/sys/class/thermal").into_iter().any(|zone| {
        let read = |name: &str| read_trimmed(&zone.join(name));
        let Some(temp) = read("temp").and_then(|t| t.parse::<i64>().ok()) else {
            return false;
        };
        (0..16).any(|i| {
            read(&format!("trip_point_{i}_type")).as_deref() == Some("critical")
                && read(&format!("trip_point_{i}_temp"))
                    .and_then(|t| t.parse::<i64>().ok())
                    .is_some_and(|crit| temp >= crit - 5000)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_lid_state() {
        assert_eq!(parse_lid("state:      closed"), Some(true));
        assert_eq!(parse_lid("state:      open"), Some(false));
        assert_eq!(parse_lid(""), None);
    }
}
