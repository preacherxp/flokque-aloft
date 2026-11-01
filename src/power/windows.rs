use super::{output, Battery, Readings};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use windows_sys::Win32::System::Power::{
    GetSystemPowerStatus, SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
    SYSTEM_POWER_STATUS,
};

/// The lid action the power plan had before Aloft set it to "Do nothing", so
/// it can be put back, also after a crash.
const SAVED_LID_ACTIONS: &str = "lid-actions";

/// Closing the lid does nothing, and a thread asks Windows to stay awake.
pub struct Hold {
    awake: Option<(mpsc::Sender<()>, JoinHandle<()>)>,
    saved: PathBuf,
    original: (u32, u32),
}

pub fn hold(dir: &Path) -> Result<Hold, String> {
    let refused = || "Couldn't change what closing the lid does".to_string();
    let saved = dir.join(SAVED_LID_ACTIONS);
    let original = match fs::read_to_string(&saved)
        .ok()
        .and_then(|s| parse_saved(&s))
    {
        Some(original) => original,
        None => lid_actions().ok_or_else(refused)?,
    };
    fs::create_dir_all(dir)
        .and_then(|_| fs::write(&saved, format!("{} {}", original.0, original.1)))
        .map_err(|_| refused())?;
    if !set_lid_actions((0, 0)) {
        let _ = fs::remove_file(&saved);
        return Err(refused());
    }
    // The execution state belongs to the thread that sets it, so one thread keeps it.
    let (stop, stopped) = mpsc::channel::<()>();
    let thread = thread::spawn(move || unsafe {
        SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
        let _ = stopped.recv();
        SetThreadExecutionState(ES_CONTINUOUS);
    });
    Ok(Hold {
        awake: Some((stop, thread)),
        saved,
        original,
    })
}

impl Hold {
    pub fn detail(&self) -> &'static str {
        "Lid-safe"
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if let Some((stop, thread)) = self.awake.take() {
            drop(stop);
            let _ = thread.join();
        }
        if set_lid_actions(self.original) {
            let _ = fs::remove_file(&self.saved);
        }
    }
}

/// Puts back a lid action left behind when Aloft didn't quit properly.
pub fn recover(dir: &Path) {
    let saved = dir.join(SAVED_LID_ACTIONS);
    if let Some(original) = fs::read_to_string(&saved)
        .ok()
        .and_then(|s| parse_saved(&s))
    {
        if set_lid_actions(original) {
            let _ = fs::remove_file(&saved);
        }
    }
}

pub fn read() -> Readings {
    Readings {
        battery: battery(),
        lid_closed: None,
        overheating: false,
    }
}

fn battery() -> Option<Battery> {
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return None;
    }
    const NO_BATTERY: u8 = 128;
    const UNKNOWN: u8 = 255;
    if status.BatteryFlag & NO_BATTERY != 0 || status.BatteryLifePercent == UNKNOWN {
        return None;
    }
    Some(Battery {
        percent: status.BatteryLifePercent,
        on_battery: status.ACLineStatus == 0,
    })
}

/// The current plan's lid close action, plugged in and on battery.
fn lid_actions() -> Option<(u32, u32)> {
    parse_lid_actions(&output(
        "powercfg",
        &["/query", "SCHEME_CURRENT", "SUB_BUTTONS", "LIDACTION"],
    )?)
}

fn set_lid_actions((ac, dc): (u32, u32)) -> bool {
    let set = |flag, value: u32| {
        output(
            "powercfg",
            &[
                flag,
                "SCHEME_CURRENT",
                "SUB_BUTTONS",
                "LIDACTION",
                &value.to_string(),
            ],
        )
        .is_some()
    };
    set("/setacvalueindex", ac)
        && set("/setdcvalueindex", dc)
        && output("powercfg", &["/setactive", "SCHEME_CURRENT"]).is_some()
}

/// powercfg's labels are translated, so this goes by the last two hex values:
/// the current AC, then DC, setting index.
fn parse_lid_actions(query: &str) -> Option<(u32, u32)> {
    let values: Vec<u32> = query
        .lines()
        .filter_map(|line| {
            line.rsplit_once(':')?
                .1
                .trim()
                .strip_prefix("0x")
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        })
        .collect();
    match values[..] {
        [.., ac, dc] => Some((ac, dc)),
        _ => None,
    }
}

fn parse_saved(saved: &str) -> Option<(u32, u32)> {
    let (ac, dc) = saved.trim().split_once(' ')?;
    Some((ac.parse().ok()?, dc.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_lid_actions_from_powercfg() {
        let query = "Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Balanced)\n  Subgroup GUID: 4f971e89-eebd-4455-a8de-9e59040e7347  (Power buttons and lid)\n    Power Setting GUID: 5ca83367-6e45-459f-a27b-476b1d01c936  (Lid close action)\n      Possible Setting Index: 000\n      Possible Setting Friendly Name: Do nothing\n    Current AC Power Setting Index: 0x00000001\n    Current DC Power Setting Index: 0x00000002\n";
        assert_eq!(parse_lid_actions(query), Some((1, 2)));
        assert_eq!(parse_lid_actions(""), None);
    }
}
