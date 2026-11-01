// No console window next to the tray icon on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod flokque;
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
    /// Flokque's name, kept while it doesn't answer.
    server: Option<String>,
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
    /// "online" or "offline"; None when Flokque hasn't answered since Aloft started.
    flokque: Option<&'static str>,
    server: Option<String>,
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
            flokque: match (&self.flokque, &self.server) {
                (Some(_), _) => Some("online"),
                (None, Some(_)) => Some("offline"),
                (None, None) => None,
            },
            server: self.server.clone(),
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
        let _ = tray.icon.set_icon_as_template(true);
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
        if let Some(name) = health.as_ref().and_then(|health| health.name.clone()) {
            aloft.server = Some(name);
        } else if health.is_some() && aloft.server.is_none() {
            aloft.server = Some("Flokque".into());
        }
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

fn tray_icon(on: bool) -> Image<'static> {
    let png: &[u8] = if on {
        include_bytes!("../icons/tray-on.png")
    } else {
        include_bytes!("../icons/tray-off.png")
    };
    let bird = Image::from_bytes(png).expect("tray icons are PNGs");
    // A template image, which macOS colours for the menu bar.
    if cfg!(target_os = "macos") {
        return bird;
    }
    tile(&bird)
}

/// Trays elsewhere can be light or dark: a white bird on a Flokque-teal tile shows on both.
fn tile(bird: &Image) -> Image<'static> {
    const SIZE: u32 = 72;
    const RADIUS: f64 = 16.0;
    const TEAL: [u8; 3] = [0x1f, 0x5a, 0x6b];
    let (width, height) = (bird.width(), bird.height());
    let (left, top) = ((SIZE - width) / 2, (SIZE - height) / 2);
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (cx, cy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let far = f64::from(SIZE) - RADIUS;
            let dx = (RADIUS - cx).max(cx - far).max(0.0);
            let dy = (RADIUS - cy).max(cy - far).max(0.0);
            if dx * dx + dy * dy > RADIUS * RADIUS {
                continue;
            }
            let inside = (left..left + width).contains(&x) && (top..top + height).contains(&y);
            let alpha = if inside {
                bird.rgba()[(((y - top) * width + (x - left)) * 4 + 3) as usize]
            } else {
                0
            };
            let a = f64::from(alpha) / 255.0;
            let i = ((y * SIZE + x) * 4) as usize;
            for (c, teal) in TEAL.iter().enumerate() {
                rgba[i + c] = (f64::from(*teal) * (1.0 - a) + 255.0 * a).round() as u8;
            }
            rgba[i + 3] = 255;
        }
    }
    Image::new_owned(rgba, SIZE, SIZE)
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
                server: None,
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
                    "show" => {
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
    fn tiles_the_bird_with_rounded_corners() {
        let tile = tile(&Image::from_bytes(include_bytes!("../icons/tray-on.png")).unwrap());
        let pixel = |x: u32, y: u32| &tile.rgba()[((y * tile.width() + x) * 4) as usize..][..4];
        assert_eq!(pixel(0, 0)[3], 0, "the corner is cut");
        assert_eq!(pixel(36, 2), &[0x1f, 0x5a, 0x6b, 255], "the tile is teal");
    }
}
