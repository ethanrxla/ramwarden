//! Native, color-keyed companion windows. No graphics driver or extra process.
use super::{Pane, activation::Surface};
use crate::{
    companions::{self, Options, Style},
    wardens::Sprite,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{ReleaseCapture, SetCapture},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

const WIDTH: i32 = 180;
const HEIGHT: i32 = 180;
const KEY: COLORREF = COLORREF(0xff00ff);

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    ram: bool,
    threat: bool,
    positions: [Option<[i32; 2]>; 2],
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            ram: true,
            threat: true,
            positions: [None, None],
        }
    }
}
struct Store {
    prefs: Preferences,
    path: PathBuf,
}
impl Store {
    fn save(&self) {
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let pending = self.path.with_extension("tmp");
            std::fs::write(&pending, serde_json::to_vec(&self.prefs)?)?;
            std::fs::rename(pending, &self.path)
        })();
        if let Err(error) = result {
            tracing::warn!(%error,"could not save companion preferences");
        }
    }
}
struct Pet {
    pane: Pane,
    sprite: Sprite,
    style: Style,
    store: Arc<Mutex<Store>>,
    clicks: mpsc::Sender<(Pane, Surface)>,
    phase: u64,
    preview: bool,
    drag: Option<(POINT, RECT)>,
    moved: bool,
    wheel: i32,
}
impl Pet {
    fn index(&self) -> usize {
        if self.pane == Pane::Ram { 0 } else { 1 }
    }
}

pub struct Pets {
    windows: [HWND; 2],
    store: Arc<Mutex<Store>>,
    clicks: mpsc::Receiver<(Pane, Surface)>,
}
impl Pets {
    pub fn new() -> Result<Self, String> {
        Self::create(false)
    }
    /// Expose taskbar entries only in the visual test fixture so desktop
    /// accessibility tools can discover both otherwise tool-only windows.
    pub fn preview() -> Result<Self, String> {
        Self::create(true)
    }
    fn create(preview: bool) -> Result<Self, String> {
        let path = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("ramwarden/companions.json");
        let prefs = std::fs::read(&path)
            .ok()
            .filter(|b| b.len() < 4096)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let store = Arc::new(Mutex::new(Store { prefs, path }));
        let (tx, rx) = mpsc::channel();
        // SAFETY: this UI thread owns the registered class and both HWNDs.
        unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance.into(),
                lpszClassName: w!("RamWardenCompanionV1"),
                hCursor: LoadCursorW(None, IDC_HAND).map_err(|e| e.to_string())?,
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err("Could not register companion windows".into());
            }
            let mut windows = [HWND::default(); 2];
            for (index, pane, source, title) in [
                (
                    0,
                    Pane::Ram,
                    include_str!("../../../../web/assets/pets/ram.svg"),
                    w!("RamWarden companion"),
                ),
                (
                    1,
                    Pane::Threat,
                    companions::THREAT_SVG,
                    w!("ThreatWarden companion"),
                ),
            ] {
                let _ = source;
                let style = if pane == Pane::Ram {
                    Options::load().ram
                } else {
                    Options::load().threat
                };
                let mut style = style;
                style.scale = style.scale.clamp(2, 6);
                let dimensions = companions::panel_size(pane == Pane::Threat, style.scale);
                let sprite = if pane == Pane::Ram {
                    companions::ram(1, false, false, &style.appearance)
                } else {
                    companions::threat(&style.appearance)
                };
                let position = store.lock().unwrap().prefs.positions[index].unwrap_or([
                    GetSystemMetrics(SM_CXSCREEN) - 180 - index as i32 * 170,
                    GetSystemMetrics(SM_CYSCREEN) - 230,
                ]);
                let position = clamp_position(position);
                let pet = Pet {
                    pane,
                    sprite,
                    style,
                    store: store.clone(),
                    clicks: tx.clone(),
                    phase: 0,
                    preview,
                    drag: None,
                    moved: false,
                    wheel: 0,
                };
                // WM_NCCREATE stores this allocation; WM_NCDESTROY releases it.
                let raw = Box::into_raw(Box::new(RefCell::new(pet)));
                match CreateWindowExW(
                    WS_EX_LAYERED
                        | WS_EX_TOPMOST
                        | WS_EX_NOACTIVATE
                        | if preview {
                            WS_EX_APPWINDOW
                        } else {
                            WS_EX_TOOLWINDOW
                        },
                    w!("RamWardenCompanionV1"),
                    title,
                    WS_POPUP,
                    position[0],
                    position[1],
                    dimensions[0],
                    dimensions[1],
                    None,
                    None,
                    Some(instance.into()),
                    Some(raw.cast()),
                ) {
                    Ok(hwnd) => {
                        windows[index] = hwnd;
                        SetLayeredWindowAttributes(hwnd, KEY, 255, LWA_COLORKEY)
                            .map_err(|e| e.to_string())?;
                        if pane == Pane::Ram {
                            SetTimer(Some(hwnd), 1, 250, None);
                        }
                    }
                    Err(error) => {
                        for hwnd in windows {
                            if !hwnd.is_invalid() {
                                let _ = DestroyWindow(hwnd);
                            }
                        }
                        return Err(error.to_string());
                    }
                }
            }
            Ok(Self {
                windows,
                store,
                clicks: rx,
            })
        }
    }
    pub fn visibility(&self) -> [bool; 2] {
        let p = &self.store.lock().unwrap().prefs;
        [p.ram, p.threat]
    }
    pub fn toggle(&self, pane: Pane) {
        let mut s = self.store.lock().unwrap();
        if pane == Pane::Ram {
            s.prefs.ram = !s.prefs.ram;
        } else {
            s.prefs.threat = !s.prefs.threat;
        }
        s.save();
    }
    pub fn refresh(&self, pro: bool) {
        let options = Options::load();
        let [ram, threat] = self.visibility();
        // SAFETY: HWNDs belong to this UI thread. No activation or focus stealing.
        unsafe {
            for (hwnd, visible) in self.windows.into_iter().zip([ram, threat && pro]) {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RefCell<Pet>;
                if !ptr.is_null() {
                    let style = if hwnd == self.windows[0] {
                        options.ram.clone()
                    } else {
                        options.threat.clone()
                    };
                    let mut resized = None;
                    if let Ok(mut pet) = (&*ptr).try_borrow_mut()
                        && pet.style != style
                    {
                        pet.style = style;
                        pet.style.scale = pet.style.scale.clamp(2, 6);
                        pet.sprite = if pet.pane == Pane::Ram {
                            companions::ram(pet.phase, false, false, &pet.style.appearance)
                        } else {
                            companions::threat(&pet.style.appearance)
                        };
                        resized = Some(companions::panel_size(
                            pet.pane == Pane::Threat,
                            pet.style.scale,
                        ));
                    }
                    if let Some([width, height]) = resized {
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            0,
                            0,
                            width,
                            height,
                            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                let _ = ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
        }
    }
    pub fn click(&self) -> Option<(Pane, Surface)> {
        self.clicks.try_recv().ok()
    }
}
impl Drop for Pets {
    fn drop(&mut self) {
        // SAFETY: dropping the owner on the message-loop thread releases each pet.
        unsafe {
            for hwnd in self.windows {
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}

fn wheel_scale(scale: u32, steps: i32) -> u32 {
    (i64::from(scale) + i64::from(steps)).clamp(2, 6) as u32
}

fn clamp_position([x, y]: [i32; 2]) -> [i32; 2] {
    // SAFETY: read-only virtual desktop metrics, including negative monitor origins.
    unsafe {
        let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
        [
            x.clamp(
                left,
                (left + GetSystemMetrics(SM_CXVIRTUALSCREEN) - WIDTH).max(left),
            ),
            y.clamp(
                top,
                (top + GetSystemMetrics(SM_CYVIRTUALSCREEN) - HEIGHT).max(top),
            ),
        ]
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: Win32 invokes this on the owning thread. The Box pointer is set
    // during WM_NCCREATE and remains live until WM_NCDESTROY clears it.
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, message, wp, lp);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RefCell<Pet>;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, message, wp, lp);
        }
        if message == WM_NCDESTROY {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(ptr));
            return DefWindowProcW(hwnd, message, wp, lp);
        }
        let Ok(mut pet) = (&*ptr).try_borrow_mut() else {
            return DefWindowProcW(hwnd, message, wp, lp);
        };
        match message {
            WM_TIMER => {
                if IsWindowVisible(hwnd).as_bool() {
                    pet.phase = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                        .saturating_sub(978307200000) as u64
                        / 250;
                    if pet.pane == Pane::Ram {
                        pet.sprite =
                            companions::ram(pet.phase, false, false, &pet.style.appearance);
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let screen = BeginPaint(hwnd, &mut paint);
                let mut bounds = RECT::default();
                let _ = GetClientRect(hwnd, &mut bounds);
                let dc = CreateCompatibleDC(Some(screen));
                let bitmap = CreateCompatibleBitmap(screen, bounds.right, bounds.bottom);
                let previous = SelectObject(dc, bitmap.into());
                let background = CreateSolidBrush(KEY);
                FillRect(
                    dc,
                    &RECT {
                        left: 0,
                        top: 0,
                        right: bounds.right,
                        bottom: bounds.bottom,
                    },
                    background,
                );
                let _ = DeleteObject(background.into());
                let bob = if pet.pane == Pane::Ram {
                    companions::ram_bob(pet.phase, false)
                } else {
                    18
                };
                let scale = pet.style.scale as i32;
                let origin_x = if pet.pane == Pane::Ram { 18 } else { 8 };
                for y in 0..pet.sprite.height {
                    for x in 0..pet.sprite.width {
                        let i = ((y * pet.sprite.width + x) * 4) as usize;
                        let pixel = &pet.sprite.rgba[i..i + 4];
                        if pixel[3] == 0 {
                            continue;
                        }
                        let brush = CreateSolidBrush(COLORREF(
                            pixel[0] as u32 | (pixel[1] as u32) << 8 | (pixel[2] as u32) << 16,
                        ));
                        FillRect(
                            dc,
                            &RECT {
                                left: origin_x + x as i32 * scale,
                                top: y as i32 * scale + bob,
                                right: origin_x + (x as i32 + 1) * scale,
                                bottom: (y as i32 + 1) * scale + bob,
                            },
                            brush,
                        );
                        let _ = DeleteObject(brush.into());
                    }
                }
                // Publish a complete frame once. Painting the color-key background
                // directly onto the layered window exposed the desktop each tick.
                let _ = BitBlt(
                    screen,
                    0,
                    0,
                    bounds.right,
                    bounds.bottom,
                    Some(dc),
                    0,
                    0,
                    SRCCOPY,
                );
                SelectObject(dc, previous);
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(dc);
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                pet.wheel += ((wp.0 >> 16) as u16 as i16) as i32;
                let steps = pet.wheel / 120;
                pet.wheel %= 120;
                if steps != 0 {
                    let mut options = Options::load();
                    let style = if pet.pane == Pane::Ram {
                        &mut options.ram
                    } else {
                        &mut options.threat
                    };
                    style.scale = wheel_scale(style.scale, steps);
                    let scale = style.scale;
                    if options.save().is_ok() {
                        pet.style.scale = scale;
                        let [width, height] =
                            companions::panel_size(pet.pane == Pane::Threat, scale);
                        drop(pet);
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            0,
                            0,
                            width,
                            height,
                            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                LRESULT(0)
            }
            WM_CONTEXTMENU => {
                if let Ok(menu) = CreatePopupMenu() {
                    let _ = AppendMenuW(menu, MF_STRING, 1, w!("Chat"));
                    let _ = AppendMenuW(menu, MF_STRING, 2, w!("Settings…"));
                    let _ = AppendMenuW(menu, MF_STRING, 3, w!("Hide companion"));
                    let mut point = POINT::default();
                    let _ = GetCursorPos(&mut point);
                    // Release the RefCell before the modal native menu pumps messages.
                    drop(pet);
                    let _ = SetForegroundWindow(hwnd);
                    let selected = TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_RIGHTBUTTON,
                        point.x,
                        point.y,
                        None,
                        hwnd,
                        None,
                    )
                    .0;
                    let _ = DestroyMenu(menu);
                    if let Ok(pet) = (&*ptr).try_borrow() {
                        match selected {
                            1 => {
                                let _ = pet.clicks.send((pet.pane, Surface::Chat));
                            }
                            2 => {
                                let _ = pet.clicks.send((pet.pane, Surface::Settings));
                            }
                            3 => {
                                let mut s = pet.store.lock().unwrap();
                                if pet.pane == Pane::Ram {
                                    s.prefs.ram = false
                                } else {
                                    s.prefs.threat = false
                                };
                                s.save();
                                let _ = ShowWindow(hwnd, SW_HIDE);
                            }
                            _ => {}
                        }
                    }
                }
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_LBUTTONDOWN => {
                if pet.preview {
                    eprintln!("pet pointer down");
                }
                let mut point = POINT::default();
                let mut rect = RECT::default();
                if GetCursorPos(&mut point).is_ok() && GetWindowRect(hwnd, &mut rect).is_ok() {
                    pet.drag = Some((point, rect));
                    pet.moved = false;
                    SetCapture(hwnd);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if let Some((start, rect)) = pet.drag {
                    let mut point = POINT::default();
                    if GetCursorPos(&mut point).is_ok() {
                        let dx = point.x - start.x;
                        let dy = point.y - start.y;
                        if pet.moved || dx.abs() + dy.abs() > 5 {
                            pet.moved = true;
                            let p = clamp_position([rect.left + dx, rect.top + dy]);
                            let _ = SetWindowPos(
                                hwnd,
                                None,
                                p[0],
                                p[1],
                                0,
                                0,
                                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                            );
                        }
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if pet.drag.take().is_some() {
                    let _ = ReleaseCapture();
                    if pet.moved {
                        if pet.preview {
                            eprintln!("pet drag saved");
                        }
                        let mut rect = RECT::default();
                        if GetWindowRect(hwnd, &mut rect).is_ok() {
                            let mut s = pet.store.lock().unwrap();
                            s.prefs.positions[pet.index()] = Some([rect.left, rect.top]);
                            s.save();
                        }
                    } else {
                        let _ = pet.clicks.send((pet.pane, Surface::Chat));
                    }
                }
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                pet.drag = None;
                LRESULT(0)
            }
            WM_CLOSE => {
                let mut s = pet.store.lock().unwrap();
                if pet.pane == Pane::Ram {
                    s.prefs.ram = false;
                } else {
                    s.prefs.threat = false;
                }
                s.save();
                let _ = ShowWindow(hwnd, SW_HIDE);
                LRESULT(0)
            }
            _ => {
                drop(pet);
                DefWindowProcW(hwnd, message, wp, lp)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wheel_size_matches_mac_integer_bounds() {
        assert_eq!(wheel_scale(3, 1), 4);
        assert_eq!(wheel_scale(3, -1), 2);
        assert_eq!(wheel_scale(6, 10), 6);
        assert_eq!(wheel_scale(2, -10), 2);
    }
    #[test]
    fn preferences_default_visible_and_restore_independent_positions() {
        let p: Preferences =
            serde_json::from_str(r#"{"ram":false,"positions":[[30,40],null]}"#).unwrap();
        assert!(!p.ram);
        assert!(p.threat);
        assert_eq!(p.positions[0], Some([30, 40]));
    }
    #[test]
    fn saved_visibility_and_positions_survive_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("companions.json");
        let prefs = Preferences {
            ram: false,
            threat: true,
            positions: [Some([42, 63]), Some([222, 111])],
        };
        Store {
            prefs,
            path: path.clone(),
        }
        .save();
        let restored: Preferences = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(!restored.ram);
        assert!(restored.threat);
        assert_eq!(restored.positions, [Some([42, 63]), Some([222, 111])]);
    }
}

/// Position an attached box on the virtual desktop, preferring above the pet.
/// Coordinates use the same desktop coordinate space as persisted native pets.
pub fn attachment(pane: Pane, size: [f32; 2], prefer_above: bool) -> ([f32; 2], bool) {
    let path = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("ramwarden/companions.json");
    let prefs: Preferences = std::fs::read(path)
        .ok()
        .filter(|b| b.len() < 4096)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let index = usize::from(pane == Pane::Threat);
    let options = Options::load();
    let scale = if index == 0 {
        options.ram.scale
    } else {
        options.threat.scale
    };
    let pet_size = companions::panel_size(index == 1, scale);
    // SAFETY: read-only virtual desktop metrics; no window manipulation.
    unsafe {
        let mut position = prefs.positions[index].unwrap_or([
            GetSystemMetrics(SM_CXSCREEN) - 180 - index as i32 * 170,
            GetSystemMetrics(SM_CYSCREEN) - 230,
        ]);
        let mut pet_size = pet_size;
        let mut bounds = [
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        ];
        // Query the live native pet in this DPI-aware process. Persisted tray
        // coordinates may be virtualized; using them as physical pixels clips boxes.
        if let Ok(hwnd) = FindWindowW(
            w!("RamWardenCompanionV1"),
            if index == 0 {
                w!("RamWarden companion")
            } else {
                w!("ThreatWarden companion")
            },
        ) {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_ok() {
                position = [rect.left, rect.top];
                pet_size = [rect.right - rect.left, rect.bottom - rect.top];
            }
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(monitor, &mut info).as_bool() {
                let r = info.rcWork;
                bounds = [r.left, r.top, r.right - r.left, r.bottom - r.top];
            }
        }
        attached_position(position, pet_size, size, bounds, prefer_above)
    }
}
fn attached_position(
    pet: [i32; 2],
    pet_size: [i32; 2],
    size: [f32; 2],
    bounds: [i32; 4],
    prefer_above: bool,
) -> ([f32; 2], bool) {
    let [left, top, width, height] = bounds.map(|v| v as f32);
    let x = pet[0] as f32 + (pet_size[0] as f32 - size[0]) / 2.;
    let above = pet[1] as f32 - size[1] - 4.;
    let below = (pet[1] + pet_size[1] + 4) as f32;
    let y = if prefer_above {
        if above >= top { above } else { below }
    } else if below + size[1] <= top + height {
        below
    } else {
        above
    };
    let position = [
        x.clamp(left, (left + width - size[0]).max(left)),
        y.clamp(top, (top + height - size[1]).max(top)),
    ];
    (position, position[1] + size[1] <= pet[1] as f32)
}
#[cfg(test)]
mod attachment_tests {
    use super::*;
    #[test]
    fn above_below_and_negative_monitor_coordinates() {
        assert_eq!(
            attached_position(
                [100, 500],
                [180, 180],
                [300., 200.],
                [0, 0, 1920, 1080],
                false
            )
            .0,
            [40., 684.]
        );
        assert_eq!(
            attached_position(
                [100, 500],
                [180, 180],
                [300., 200.],
                [0, 0, 1920, 1080],
                true
            )
            .0,
            [40., 296.]
        );
        assert_eq!(
            attached_position(
                [100, 10],
                [180, 180],
                [300., 200.],
                [0, 0, 1920, 1080],
                true
            )
            .0,
            [40., 194.]
        );
        assert!(
            attached_position(
                [-1800, 400],
                [180, 180],
                [300., 200.],
                [-1920, 0, 3840, 1080],
                true
            )
            .0[0]
                < 0.
        );
    }
}
