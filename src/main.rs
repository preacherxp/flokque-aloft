// No console window next to the tray icon on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod flokque;
#[cfg(target_os = "macos")]
mod menu_bar_check;
mod power;
mod settings;

use serde::Serialize;
use settings::Settings;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, Rect, RunEvent, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};

const PANEL: &str = "panel";
const TRAY: &str = "aloft";

/// Keeps the computer working with its lid closed, and turns that off when
/// it's too hot or the battery runs low. Shows Flokque's agents when Flokque
/// runs here.
struct Aloft {
    dir: PathBuf,
    hold: Option<power::Hold>,
    problem: Option<String>,
    readings: power::Readings,
    flokque: Option<flokque::Health>,
}

struct Tray {
    icon: TrayIcon,
    keep: CheckMenuItem<tauri::Wry>,
    shown_on: Mutex<Option<bool>>,
}

/// When the panel last hid, kept apart from Aloft so window events never wait on it.
struct PanelHiddenAt(Mutex<Option<Instant>>);

/// What the panel shows.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct View {
    on: bool,
    detail: String,
    problem: bool,
    /// Agents Flokque is running; None while no Flokque answers here.
    agents: Option<u32>,
    lid: Option<&'static str>,
    power: String,
    can_open_flokque: bool,
}

impl Aloft {
    fn view(&self) -> View {
        let detail = match (&self.problem, &self.hold) {
            (Some(problem), _) => problem.clone(),
            (None, Some(hold)) => hold.detail().into(),
            (None, None) => "Sleeps when the lid closes".into(),
        };
        View {
            on: self.hold.is_some(),
            detail,
            problem: self.problem.is_some(),
            agents: self.flokque.as_ref().map(|health| health.active_sessions),
            lid: self
                .readings
                .lid_closed
                .map(|closed| if closed { "Closed" } else { "Open" }),
            power: match self.readings.battery {
                Some(battery) => format!(
                    "{}% · {}",
                    battery.percent,
                    if battery.on_battery {
                        "on battery"
                    } else {
                        "plugged in"
                    }
                ),
                None => "Plugged in".into(),
            },
            // Opens the Flokque app, which only Macs have.
            can_open_flokque: cfg!(target_os = "macos") && self.flokque.is_some(),
        }
    }
}

fn aloft(app: &AppHandle) -> MutexGuard<'_, Aloft> {
    app.state::<Mutex<Aloft>>()
        .inner()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Turns keeping aloft on or off. On the main thread, which the Mac's virtual display needs.
fn set(app: &AppHandle, want: bool) {
    let (dir, held) = {
        let mut aloft = aloft(app);
        (aloft.dir.clone(), aloft.hold.take())
    };
    // Made and dropped outside the lock: either can take a while, an admin prompt even.
    let (hold, problem) = match (want, held) {
        (false, _) => (None, None),
        (true, Some(hold)) => (Some(hold), None),
        (true, None) => match power::hold(&dir) {
            Ok(hold) => (Some(hold), None),
            Err(problem) => (None, Some(problem)),
        },
    };
    if hold.is_some() == want {
        Settings::save_on(&dir, want);
    }
    {
        let mut aloft = aloft(app);
        aloft.hold = hold;
        aloft.problem = problem;
    }
    refresh(app);
}

/// Brings the tray and the panel up to date.
fn refresh(app: &AppHandle) {
    let view = aloft(app).view();
    let tray = app.state::<Tray>();
    let _ = tray.keep.set_checked(view.on);
    let mut shown_on = tray
        .shown_on
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if *shown_on != Some(view.on) {
        let _ = tray.icon.set_icon(Some(tray_icon(view.on)));
        #[cfg(target_os = "macos")]
        {
            let _ = tray.icon.set_icon_as_template(true);
            let on = view.on;
            let _ = tray.icon.with_inner_tray_icon(move |inner| {
                if let Some(item) = inner.ns_status_item() {
                    menu_bar_check::show(&item, on);
                }
            });
        }
        *shown_on = Some(view.on);
    }
    let _ = app.emit_to(PANEL, "state", view);
}

/// Every 10 seconds: the battery, lid and temperature, and Flokque if it runs here.
fn watch(app: AppHandle) {
    let dir = aloft(&app).dir.clone();
    loop {
        let settings = Settings::load(&dir);
        let readings = power::read();
        let health = flokque::health(&settings.flokque_url);
        let handle = app.clone();
        let _ =
            app.run_on_main_thread(move || check(&handle, readings, health, settings.min_battery));
        thread::sleep(Duration::from_secs(10));
    }
}

fn check(
    app: &AppHandle,
    readings: power::Readings,
    health: Option<flokque::Health>,
    min_battery: u8,
) {
    let stopped = {
        let mut aloft = aloft(app);
        aloft.readings = readings;
        aloft.flokque = health;
        match unsafe_to_stay_awake(&readings, min_battery) {
            Some(reason) if aloft.hold.is_some() => {
                aloft.problem = Some(format!("Turned off: {reason}"));
                aloft.hold.take()
            }
            _ => None,
        }
    };
    if stopped.is_some() {
        drop(stopped);
        Settings::save_on(&aloft(app).dir.clone(), false);
    }
    refresh(app);
}

fn unsafe_to_stay_awake(readings: &power::Readings, min_battery: u8) -> Option<String> {
    if readings.overheating {
        return Some(
            if cfg!(target_os = "macos") {
                "Mac is overheating"
            } else {
                "overheating"
            }
            .into(),
        );
    }
    match readings.battery {
        Some(battery) if battery.on_battery && battery.percent < min_battery => {
            Some(format!("battery below {min_battery}%"))
        }
        _ => None,
    }
}

/// On macOS, two rising arrows as a template image, which macOS colours for the
/// menu bar, with menu_bar_check adding the green check while on. Elsewhere,
/// the colour app icon with the check drawn in.
fn tray_icon(on: bool) -> Image<'static> {
    let png: &[u8] = match (cfg!(target_os = "macos"), on) {
        (true, true) => include_bytes!("../icons/menu-bar-on.png"),
        (true, false) => include_bytes!("../icons/menu-bar-off.png"),
        (false, true) => include_bytes!("../icons/tray-on.png"),
        (false, false) => include_bytes!("../icons/tray-off.png"),
    };
    Image::from_bytes(png).expect("tray icons are PNGs")
}

/// Under the tray icon at the top of the screen, above it at the bottom, or top
/// right where the tray doesn't say where it is (Linux).
fn place(panel: &WebviewWindow, anchor: Option<Rect>) -> Option<()> {
    let scale = panel.scale_factor().ok()?;
    let size = panel.outer_size().ok()?;
    let (width, height) = (f64::from(size.width), f64::from(size.height));
    let margin = 8.0 * scale;
    let icon = anchor.map(|rect| {
        (
            rect.position.to_physical::<f64>(scale),
            rect.size.to_physical::<f64>(scale),
        )
    });
    let monitor = match icon {
        Some((position, _)) => panel
            .monitor_from_point(position.x, position.y)
            .ok()
            .flatten(),
        None => None,
    }
    .or_else(|| panel.primary_monitor().ok().flatten())?;
    let area = monitor.work_area();
    let (area_left, area_top) = (f64::from(area.position.x), f64::from(area.position.y));
    let (area_right, area_bottom) = (
        area_left + f64::from(area.size.width),
        area_top + f64::from(area.size.height),
    );
    let (x, y) = match icon {
        Some((position, icon)) => {
            let x = position.x + icon.width / 2.0 - width / 2.0;
            let below = position.y - area_top < area_bottom - position.y;
            (
                x,
                if below {
                    position.y + icon.height + margin
                } else {
                    position.y - height - margin
                },
            )
        }
        None => (area_right - width - margin, area_top + margin),
    };
    let x = x.min(area_right - width - margin).max(area_left + margin);
    panel
        .set_position(PhysicalPosition::new(x, y.max(area_top)))
        .ok()
}

fn show_panel(app: &AppHandle, anchor: Option<Rect>) {
    let Some(panel) = app.get_webview_window(PANEL) else {
        return;
    };
    place(&panel, anchor);
    let _ = panel.show();
    let _ = panel.set_focus();
}

fn hide_panel(app: &AppHandle) {
    if let Some(panel) = app.get_webview_window(PANEL) {
        let _ = panel.hide();
    }
    *app.state::<PanelHiddenAt>()
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
}

fn toggle_panel(app: &AppHandle, anchor: Rect) {
    let visible = app
        .get_webview_window(PANEL)
        .is_some_and(|panel| panel.is_visible().unwrap_or(false));
    // Clicking the tray icon takes focus from the panel first, which hides it: leave it hidden.
    let hidden_at = *app
        .state::<PanelHiddenAt>()
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let just_hidden = hidden_at.is_some_and(|at| at.elapsed() < Duration::from_millis(300));
    if visible {
        hide_panel(app);
    } else if !just_hidden {
        show_panel(app, Some(anchor));
    }
}

#[tauri::command]
fn state(app: AppHandle) -> View {
    aloft(&app).view()
}

#[tauri::command]
fn set_on(app: AppHandle, on: bool) {
    set(&app, on);
}

#[tauri::command]
fn open_flokque(app: AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("flokque://")
        .spawn();
    hide_panel(&app);
}

/// Opens one of the About page's links in the browser.
#[tauri::command]
fn open_link(app: AppHandle, link: String) {
    let url = match link.as_str() {
        "source" => "https://github.com/preacherxp/flokque-aloft",
        "coffee" => "https://buymeacoffee.com/michal.hachula",
        _ => return,
    };
    let opener = if cfg!(target_os = "macos") {
        "/usr/bin/open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(url).spawn();
    hide_panel(&app);
}

#[tauri::command]
fn hide(app: AppHandle) {
    hide_panel(&app);
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

/// launchd and systemd stop Aloft with SIGTERM: quit properly so sleep comes back.
#[cfg(unix)]
fn quit_on_signals(app: AppHandle) {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    if let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGTERM, SIGINT, SIGHUP]) {
        thread::spawn(move || {
            if signals.forever().next().is_some() {
                app.exit(0);
            }
        });
    }
}

#[cfg(not(unix))]
fn quit_on_signals(_app: AppHandle) {}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            state,
            set_on,
            open_flokque,
            open_link,
            hide,
            quit
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let dir = app.path().app_config_dir()?;
            power::recover(&dir);
            let on = Settings::load(&dir).on;
            app.manage(Mutex::new(Aloft {
                dir,
                hold: None,
                problem: None,
                readings: power::Readings::default(),
                flokque: None,
            }));
            app.manage(PanelHiddenAt(Mutex::new(None)));

            let keep = CheckMenuItem::with_id(
                app,
                "keep",
                "Keep aloft with lid closed",
                true,
                false,
                None::<&str>,
            )?;
            let menu = Menu::with_items(
                app,
                &[
                    &keep,
                    &MenuItem::with_id(app, "show", "Show Flokque Aloft", true, None::<&str>)?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "about", "About Flokque Aloft", true, None::<&str>)?,
                    &MenuItem::with_id(app, "quit", "Quit Flokque Aloft", true, None::<&str>)?,
                ],
            )?;
            let icon = TrayIconBuilder::with_id(TRAY)
                .icon(tray_icon(false))
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("Flokque Aloft")
                .menu(&menu)
                // Left click opens the panel and right click the menu; Linux trays only show the menu.
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "keep" => {
                        let on = aloft(app).hold.is_some();
                        set(app, !on);
                    }
                    "show" | "about" => {
                        let page = if event.id().as_ref() == "about" {
                            "about"
                        } else {
                            "home"
                        };
                        let _ = app.emit_to(PANEL, "page", page);
                        let anchor = app
                            .tray_by_id(TRAY)
                            .and_then(|tray| tray.rect().ok().flatten());
                        show_panel(app, anchor);
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        rect,
                        ..
                    } = event
                    {
                        toggle_panel(tray.app_handle(), rect);
                    }
                })
                .build(app)?;
            app.manage(Tray {
                icon,
                keep,
                shown_on: Mutex::new(Some(false)),
            });

            let panel = WebviewWindowBuilder::new(app, PANEL, WebviewUrl::App("index.html".into()))
                .title("Flokque Aloft")
                .inner_size(290.0, 280.0)
                .resizable(false)
                .decorations(false)
                .transparent(true)
                .always_on_top(true)
                .skip_taskbar(true)
                .visible(false)
                .build()?;
            let handle = app.handle().clone();
            panel.on_window_event(move |event| match event {
                WindowEvent::Focused(false) => hide_panel(&handle),
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    hide_panel(&handle);
                }
                _ => {}
            });

            if on {
                set(app.handle(), true);
            }
            quit_on_signals(app.handle().clone());
            let handle = app.handle().clone();
            thread::spawn(move || watch(handle));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Flokque Aloft failed to start")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                // Lets the computer sleep again.
                let hold = aloft(app).hold.take();
                drop(hold);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use power::{Battery, Readings};

    #[test]
    fn turns_off_when_hot_or_low_on_battery() {
        let battery = |percent, on_battery| Readings {
            battery: Some(Battery {
                percent,
                on_battery,
            }),
            ..Readings::default()
        };
        assert_eq!(
            unsafe_to_stay_awake(&battery(19, true), 20).as_deref(),
            Some("battery below 20%")
        );
        assert_eq!(unsafe_to_stay_awake(&battery(19, false), 20), None);
        assert_eq!(unsafe_to_stay_awake(&battery(20, true), 20), None);
        assert!(unsafe_to_stay_awake(
            &Readings {
                overheating: true,
                ..Readings::default()
            },
            20
        )
        .is_some());
        assert_eq!(unsafe_to_stay_awake(&Readings::default(), 20), None);
    }

    #[test]
    fn draws_the_check_into_the_colour_icon_only_while_on() {
        let icons: [&[u8]; 2] = [
            include_bytes!("../icons/tray-off.png"),
            include_bytes!("../icons/tray-on.png"),
        ];
        for (png, on) in icons.into_iter().zip([false, true]) {
            let icon = Image::from_bytes(png).unwrap();
            let pixel =
                |x: u32, y: u32| icon.rgba()[((y * icon.width() + x) * 4) as usize..][..4].to_vec();
            assert_eq!(pixel(0, 0)[3], 0, "the corner is clear");
            // The left of the check's disc, clear of its tick.
            let [red, green, ..] = pixel(46, 56)[..] else {
                unreachable!()
            };
            assert_eq!(green > red + 100, on, "green only while on");
        }
    }
}
