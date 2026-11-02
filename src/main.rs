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

/// The tray's bird: perched, or flying with a green check while on.
fn tray_icon(on: bool) -> Image<'static> {
    let png: &[u8] = if on {
        include_bytes!("../icons/tray-on.png")
    } else {
        include_bytes!("../icons/tray-off.png")
    };
    let bird = Image::from_bytes(png).expect("tray icons are PNGs");
    // A template image, which macOS colours for the menu bar; menu_bar_check
    // puts the check in a gap cut out of the bird's bottom right.
    #[cfg(target_os = "macos")]
    {
        if !on {
            return bird;
        }
        // tray-icon shows the bird 18 points tall.
        let per_point = f64::from(bird.height()) / 18.0;
        let radius = menu_bar_check::SIZE / 2.0 * per_point;
        let (width, height) = (f64::from(bird.width()), f64::from(bird.height()));
        let gap = (
            width - radius,
            height - radius,
            radius + menu_bar_check::GAP * per_point,
        );
        let mut rgba = bird.rgba().to_vec();
        for (i, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i as u32 % bird.width(), i as u32 / bird.width());
            if within(x, y, gap) {
                pixel[3] = 0;
            }
        }
        Image::new_owned(rgba, bird.width(), bird.height())
    }
    #[cfg(not(target_os = "macos"))]
    tile(&bird, on)
}

/// Whether a pixel's centre lies in a circle (centre x, centre y, radius).
fn within(x: u32, y: u32, (cx, cy, radius): (f64, f64, f64)) -> bool {
    let (dx, dy) = (f64::from(x) + 0.5 - cx, f64::from(y) + 0.5 - cy);
    dx * dx + dy * dy <= radius * radius
}

/// Trays elsewhere can be light or dark: a white bird on a Flokque-teal tile
/// shows on both, with the check in its bottom right corner while on.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn tile(bird: &Image, on: bool) -> Image<'static> {
    const SIZE: u32 = 72;
    const RADIUS: f64 = 16.0;
    const TEAL: [u8; 3] = [0x1f, 0x5a, 0x6b];
    const WHITE: [u8; 3] = [0xff, 0xff, 0xff];
    let check = Image::from_bytes(include_bytes!("../icons/check.png")).expect("check is a PNG");
    // Centred on the bottom right corner's curve, with a teal ring parting it from the bird.
    let check_at = SIZE - RADIUS as u32 - check.width() / 2;
    let corner = f64::from(SIZE) - RADIUS;
    let ring = (corner, corner, f64::from(check.width()) / 2.0 + 3.0);
    let bird_at = ((SIZE - bird.width()) / 2, (SIZE - bird.height()) / 2);
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (cx, cy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let dx = (RADIUS - cx).max(cx - corner).max(0.0);
            let dy = (RADIUS - cy).max(cy - corner).max(0.0);
            if dx * dx + dy * dy > RADIUS * RADIUS {
                continue;
            }
            let mut pixel = TEAL.map(f64::from);
            let mut over = |color: [u8; 3], alpha: u8| {
                let a = f64::from(alpha) / 255.0;
                for (value, color) in pixel.iter_mut().zip(color) {
                    *value = *value * (1.0 - a) + f64::from(color) * a;
                }
            };
            if !(on && within(x, y, ring)) {
                if let Some(bird) = sample(bird, x, y, bird_at) {
                    over(WHITE, bird[3]);
                }
            }
            if on {
                if let Some(check) = sample(&check, x, y, (check_at, check_at)) {
                    over([check[0], check[1], check[2]], check[3]);
                }
            }
            let i = ((y * SIZE + x) * 4) as usize;
            for (c, value) in pixel.iter().enumerate() {
                rgba[i + c] = value.round() as u8;
            }
            rgba[i + 3] = 255;
        }
    }
    Image::new_owned(rgba, SIZE, SIZE)
}

/// The image's pixel under (x, y) when it's drawn at `at`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn sample(image: &Image, x: u32, y: u32, (left, top): (u32, u32)) -> Option<[u8; 4]> {
    let (x, y) = (x.checked_sub(left)?, y.checked_sub(top)?);
    if x >= image.width() || y >= image.height() {
        return None;
    }
    let i = ((y * image.width() + x) * 4) as usize;
    image.rgba()[i..i + 4].try_into().ok()
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
    fn tiles_the_bird_with_rounded_corners_and_a_check_while_on() {
        let bird = Image::from_bytes(include_bytes!("../icons/tray-on.png")).unwrap();
        for on in [false, true] {
            let tile = tile(&bird, on);
            let pixel =
                |x: u32, y: u32| tile.rgba()[((y * tile.width() + x) * 4) as usize..][..4].to_vec();
            assert_eq!(pixel(0, 0)[3], 0, "the corner is cut");
            assert_eq!(pixel(36, 2), [0x1f, 0x5a, 0x6b, 255], "the tile is teal");
            // The left of the check's disc, clear of its tick.
            let [red, green, ..] = pixel(46, 56)[..] else {
                unreachable!()
            };
            assert_eq!(green > red + 100, on, "green only while on");
        }
    }
}
