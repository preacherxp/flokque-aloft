use super::{output, Battery, Readings};
use objc2_foundation::{NSProcessInfo, NSProcessInfoThermalState};
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// `pmset disablesleep 1`, which is what actually stops a closed lid from
/// sleeping the Mac, an idle-sleep assertion, and a virtual HDMI-like display.
/// Must be made and dropped on the main thread, for the display.
pub struct Hold {
    caffeinate: Option<Child>,
    display: Option<display::VirtualDisplay>,
}

pub fn hold(_dir: &Path) -> Result<Hold, String> {
    let sleep_disabled = pmset(true) || (install_sudo_rule() && pmset(true));
    if !sleep_disabled {
        return Err("Needs admin approval to keep running with the lid closed".into());
    }
    // -w ends the assertion with Aloft, even if Aloft crashes.
    let caffeinate = Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok();
    Ok(Hold {
        caffeinate,
        display: display::VirtualDisplay::new(),
    })
}

impl Hold {
    pub fn detail(&self) -> &'static str {
        if self.display.is_some() {
            "Virtual display on · lid-safe"
        } else {
            "Lid-safe · virtual display unavailable"
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if let Some(caffeinate) = &mut self.caffeinate {
            let _ = caffeinate.kill();
            let _ = caffeinate.wait();
        }
        pmset(false);
    }
}

pub fn recover(_dir: &Path) {}

pub fn read() -> Readings {
    Readings {
        battery: output("/usr/bin/pmset", &["-g", "batt"]).and_then(|s| parse_battery(&s)),
        lid_closed: output(
            "/usr/sbin/ioreg",
            &["-r", "-k", "AppleClamshellState", "-d", "1"],
        )
        .and_then(|s| parse_lid(&s)),
        overheating: NSProcessInfo::processInfo().thermalState()
            == NSProcessInfoThermalState::Critical,
    }
}

/// From `pmset -g batt`; None on a Mac without a battery.
fn parse_battery(pmset: &str) -> Option<Battery> {
    let line = pmset.lines().find(|l| l.contains("InternalBattery"))?;
    let percent = line
        .split('%')
        .next()?
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some(Battery {
        percent,
        on_battery: pmset.contains("'Battery Power'"),
    })
}

/// From `ioreg`; None on a Mac without a lid.
fn parse_lid(ioreg: &str) -> Option<bool> {
    match ioreg.split("\"AppleClamshellState\" = ").nth(1)? {
        rest if rest.starts_with("Yes") => Some(true),
        rest if rest.starts_with("No") => Some(false),
        _ => None,
    }
}

fn pmset(disabled: bool) -> bool {
    let value = if disabled { "1" } else { "0" };
    output(
        "/usr/bin/sudo",
        &["-n", "/usr/bin/pmset", "-a", "disablesleep", value],
    )
    .is_some()
}

/// One admin prompt installs a sudoers rule allowing only these two pmset calls.
fn install_sudo_rule() -> bool {
    let Some(user) = output("/usr/bin/id", &["-un"]) else {
        return false;
    };
    let rule = format!(
    "{} ALL=(root) NOPASSWD: /usr/bin/pmset -a disablesleep 0, /usr/bin/pmset -a disablesleep 1",
    user.trim()
  );
    let script = format!(
        r#"f=$(mktemp) && printf '%s\n' '{rule}' > "$f" && /usr/sbin/visudo -cf "$f" && /usr/bin/install -m 0440 -o root -g wheel "$f" /etc/sudoers.d/flokque-aloft; s=$?; rm -f "$f"; exit $s"#
    );
    let quoted = script.replace('\\', "\\\\").replace('"', "\\\"");
    output(
        "/usr/bin/osascript",
        &[
            "-e",
            &format!("do shell script \"{quoted}\" with administrator privileges"),
        ],
    )
    .is_some()
}

mod display {
    use objc2::msg_send;
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2_foundation::{NSArray, NSSize, NSString};
    use std::ffi::c_void;

    type DisplayConfig = *mut c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGBeginDisplayConfiguration(config: *mut DisplayConfig) -> i32;
        fn CGConfigureDisplayMirrorOfDisplay(
            config: DisplayConfig,
            display: u32,
            primary: u32,
        ) -> i32;
        fn CGCompleteDisplayConfiguration(config: DisplayConfig, option: u32) -> i32;
    }

    extern "C" {
        // dispatch_get_main_queue() is a macro for this.
        static _dispatch_main_q: c_void;
    }

    const CONFIGURE_FOR_SESSION: u32 = 1;

    /// CoreGraphics' private virtual display API, the one BetterDisplay and
    /// DeskPad use: a 1920×1080 display for as long as this lives.
    pub struct VirtualDisplay(#[allow(dead_code)] Retained<AnyObject>);

    // Only made and dropped on the main thread (see Hold).
    unsafe impl Send for VirtualDisplay {}

    impl VirtualDisplay {
        pub fn new() -> Option<Self> {
            let class = |name| AnyClass::get(name);
            unsafe {
                let descriptor: Retained<AnyObject> =
                    msg_send![class(c"CGVirtualDisplayDescriptor")?, new];
                let queue = &*(&raw const _dispatch_main_q).cast::<AnyObject>();
                let _: () = msg_send![&descriptor, setDispatchQueue: queue];
                let _: () =
                    msg_send![&descriptor, setName: &*NSString::from_str("Flokque Display")];
                let _: () = msg_send![&descriptor, setMaxPixelsWide: 1920u32];
                let _: () = msg_send![&descriptor, setMaxPixelsHigh: 1080u32];
                let _: () = msg_send![&descriptor, setSizeInMillimeters: NSSize::new(600.0, 340.0)];
                let _: () = msg_send![&descriptor, setVendorID: 0xF10Cu32];
                let _: () = msg_send![&descriptor, setProductID: 1u32];
                let _: () = msg_send![&descriptor, setSerialNum: 1u32];

                let display: Allocated<AnyObject> = msg_send![class(c"CGVirtualDisplay")?, alloc];
                let display: Option<Retained<AnyObject>> =
                    msg_send![display, initWithDescriptor: &*descriptor];
                let display = display?;

                let mode: Allocated<AnyObject> = msg_send![class(c"CGVirtualDisplayMode")?, alloc];
                let mode: Retained<AnyObject> = msg_send![mode, initWithWidth: 1920usize, height: 1080usize, refreshRate: 60.0f64];
                let settings: Retained<AnyObject> =
                    msg_send![class(c"CGVirtualDisplaySettings")?, new];
                let _: () = msg_send![&settings, setHiDPI: 0u32];
                let _: () = msg_send![&settings, setModes: &*NSArray::from_retained_slice(&[mode])];
                let applied: Bool = msg_send![&display, applySettings: &*settings];
                if !applied.as_bool() {
                    return None;
                }

                // Mirror the main display rather than extend the desktop, so no window opens on a screen nobody sees.
                let id: u32 = msg_send![&display, displayID];
                let mut config = std::ptr::null_mut();
                if CGBeginDisplayConfiguration(&mut config) == 0 {
                    CGConfigureDisplayMirrorOfDisplay(config, id, CGMainDisplayID());
                    CGCompleteDisplayConfiguration(config, CONFIGURE_FOR_SESSION);
                }
                Some(Self(display))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_battery_from_pmset() {
        let on_battery = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=4653155)\t84%; discharging; 4:12 remaining present: true\n";
        assert_eq!(
            parse_battery(on_battery),
            Some(Battery {
                percent: 84,
                on_battery: true
            })
        );
        let charging = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=4653155)\t100%; charged; 0:00 remaining present: true\n";
        assert_eq!(
            parse_battery(charging),
            Some(Battery {
                percent: 100,
                on_battery: false
            })
        );
        assert_eq!(parse_battery("Now drawing from 'AC Power'\n"), None);
    }

    #[test]
    fn reads_the_lid_from_ioreg() {
        assert_eq!(parse_lid("  \"AppleClamshellState\" = Yes\n"), Some(true));
        assert_eq!(parse_lid("  \"AppleClamshellState\" = No\n"), Some(false));
        assert_eq!(parse_lid(""), None);
    }
}
