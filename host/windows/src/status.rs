//! Notification-area ownership, independent of the app window's visibility.
//! https://learn.microsoft.com/windows/win32/shell/notification-area
#![allow(unsafe_code)]
use exact_kernel::NodeKey;
use exact_painted::{presenter::status_items::Item, Presenter};
use exact_runner::DataSource;
use std::{
    cell::{Cell, RefCell},
    ptr,
    rc::Rc,
    time::{Duration, Instant},
};
use windows_sys::{
    core::w,
    Win32::{
        Foundation::HWND,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};

mod menu;

const CALLBACK: u32 = WM_APP + 0x607;
const SUBCLASS: usize = 0x607;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Click {
    id: u16,
    primary: bool,
    point: (i32, i32),
}

struct Messages {
    restart_message: u32,
    restarted: Cell<bool>,
    alive: Cell<bool>,
    clicks: RefCell<Vec<Click>>,
}

struct Placed {
    item: Item,
    title: String,
    id: u16,
    registered: bool,
    icon: HICON,
}

pub(super) struct Status {
    hwnd: HWND,
    messages: Rc<Messages>,
    items: Vec<Placed>,
    next_id: u32,
    generation: Option<u64>,
    retry: Option<Instant>,
    reported: bool,
}

impl Status {
    pub(super) fn new(hwnd: isize) -> Result<Self, String> {
        // SAFETY: the owner installs and removes the subclass on its HWND's UI thread.
        unsafe {
            let restart_message = RegisterWindowMessageW(w!("TaskbarCreated"));
            if restart_message == 0 {
                return Err(error("register TaskbarCreated"));
            }
            let messages = Rc::new(Messages {
                restart_message,
                restarted: Cell::new(false),
                alive: Cell::new(true),
                clicks: RefCell::new(Vec::new()),
            });
            let raw = Rc::into_raw(messages.clone());
            if SetWindowSubclass(hwnd as HWND, Some(callback), SUBCLASS, raw as usize) == 0 {
                drop(Rc::from_raw(raw));
                return Err(error("install status callback"));
            }
            Ok(Self {
                hwnd: hwnd as HWND,
                messages,
                items: Vec::new(),
                next_id: 1,
                generation: None,
                retry: None,
                reported: false,
            })
        }
    }

    pub(super) fn retry_at(&self) -> Option<Instant> {
        self.retry
    }

    pub(super) fn sync<D: DataSource>(&mut self, p: &mut Presenter<D>) {
        if !self.messages.alive.get() {
            return;
        }
        if self.generation != Some(p.hosts()) {
            self.clear();
            self.messages.clicks.borrow_mut().clear();
            self.generation = Some(p.hosts());
        }
        let restarted = self.messages.restarted.replace(false);
        if restarted {
            for item in &mut self.items {
                item.registered = false;
            }
            self.retry = None;
        }
        let wanted = p.status_items();
        self.items.retain(|held| {
            if wanted.iter().any(|item| item.key == held.item.key) {
                return true;
            }
            if held.registered {
                notify(NIM_DELETE, &data(self.hwnd, held.id));
            }
            false
        });
        let icon = window_icon(self.hwnd);
        let ready = self.retry.is_none_or(|due| Instant::now() >= due);
        let mut failed = false;
        for item in wanted {
            let index = match self.items.iter().position(|held| held.item.key == item.key) {
                Some(index) => index,
                None => {
                    // Version 4 callbacks carry a 16-bit id. Never reuse an id whose
                    // queued callback could belong to a removed button.
                    let Ok(id) = u16::try_from(self.next_id) else {
                        failed = true;
                        continue;
                    };
                    self.next_id += 1;
                    self.items.push(Placed {
                        item: item.clone(),
                        title: String::new(),
                        id,
                        registered: false,
                        icon: ptr::null_mut(),
                    });
                    self.items.len() - 1
                }
            };
            let held = &mut self.items[index];
            let changed = held.title != item.title || held.icon != icon;
            held.item = item;
            if held.registered && !changed {
                continue;
            }
            if !ready {
                failed = true;
                continue;
            }
            let mut spec = data(self.hwnd, held.id);
            spec.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
            spec.uCallbackMessage = CALLBACK;
            spec.hIcon = icon;
            tip(&mut spec.szTip, &held.item.title);
            let operation = if held.registered { NIM_MODIFY } else { NIM_ADD };
            if notify(operation, &spec) {
                if !held.registered {
                    spec.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                    if !notify(NIM_SETVERSION, &spec) {
                        notify(NIM_DELETE, &spec);
                        failed = true;
                        continue;
                    }
                }
                held.registered = true;
                held.title = held.item.title.clone();
                held.icon = icon;
            } else {
                notify(NIM_DELETE, &spec);
                held.registered = false;
                failed = true;
            }
        }
        if failed && ready {
            self.retry = Some(Instant::now() + Duration::from_secs(1));
        } else if !failed {
            self.retry = None;
        }
        if failed && !self.reported {
            eprintln!("exact-windows: notification-area registration failed; retrying");
        }
        self.reported = failed;
        p.place_status_items(
            self.items
                .iter()
                .filter(|i| i.registered)
                .map(|i| i.item.key)
                .collect(),
        );
    }

    pub(super) fn service<D: DataSource + Default>(
        &mut self,
        p: &mut Presenter<D>,
        now: impl Fn() -> f64,
    ) {
        self.sync(p);
        let clicks = std::mem::take(&mut *self.messages.clicks.borrow_mut());
        for click in clicks {
            if let Some(e) = p.advance(now()) {
                p.host_mut().log(e);
            }
            self.sync(p);
            let Some(held) = self
                .items
                .iter()
                .find(|i| i.id == click.id && i.registered && i.item.enabled)
            else {
                continue;
            };
            let item = held.item.clone();
            if click.primary && item.press {
                p.activate_status_item(item.key, None);
            } else if !item.menu.is_empty() {
                let picked = menu::show(self.hwnd, &item.menu, click.point);
                if let Some(e) = p.advance(now()) {
                    p.host_mut().log(e);
                }
                match picked {
                    Ok(Some(key)) => {
                        p.activate_status_item(item.key, Some(key));
                    }
                    Ok(None) => {}
                    Err(error) => eprintln!("exact-windows: {error}"),
                }
                notify(NIM_SETFOCUS, &data(self.hwnd, click.id));
            }
            p.run_commands(D::default);
            self.sync(p);
        }
    }

    fn clear(&mut self) {
        for item in self.items.drain(..) {
            if item.registered {
                notify(NIM_DELETE, &data(self.hwnd, item.id));
            }
        }
    }
}

impl Drop for Status {
    fn drop(&mut self) {
        self.clear();
        if self.messages.alive.replace(false) {
            // SAFETY: the subclass holds one Rc until removed or WM_NCDESTROY.
            unsafe {
                if RemoveWindowSubclass(self.hwnd, Some(callback), SUBCLASS) != 0 {
                    drop(Rc::from_raw(Rc::as_ptr(&self.messages)));
                }
            }
        }
    }
}

fn data(hwnd: HWND, id: u16) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: id.into(),
        ..Default::default()
    }
}

fn notify(operation: u32, data: &NOTIFYICONDATAW) -> bool {
    // SAFETY: Shell copies the fully initialized structure during this call.
    unsafe { Shell_NotifyIconW(operation, data) != 0 }
}

fn tip(out: &mut [u16; 128], title: &str) {
    let mut at = 0;
    for ch in title.chars().take_while(|ch| *ch != '\0') {
        let mut units = [0; 2];
        let units = ch.encode_utf16(&mut units);
        if at + units.len() >= out.len() {
            break;
        }
        out[at..at + units.len()].copy_from_slice(units);
        at += units.len();
    }
}

fn window_icon(hwnd: HWND) -> HICON {
    // SAFETY: these icons are shared with the live window or the system; we do not destroy them.
    unsafe {
        let small = SendMessageW(hwnd, WM_GETICON, ICON_SMALL2 as usize, 0) as HICON;
        if !small.is_null() {
            return small;
        }
        let large = SendMessageW(hwnd, WM_GETICON, ICON_BIG as usize, 0) as HICON;
        if !large.is_null() {
            return large;
        }
        LoadIconW(ptr::null_mut(), IDI_APPLICATION)
    }
}

fn error(what: &str) -> String {
    format!(
        "notification area: {what}: {}",
        std::io::Error::last_os_error()
    )
}

unsafe extern "system" fn callback(
    hwnd: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
    subclass: usize,
    raw: usize,
) -> isize {
    // SAFETY: raw is the subclass's Rc, released once on removal or WM_NCDESTROY.
    unsafe {
        let state = &*(raw as *const Messages);
        if message == WM_NCDESTROY {
            state.alive.set(false);
            RemoveWindowSubclass(hwnd, Some(callback), subclass);
            drop(Rc::from_raw(raw as *const Messages));
        } else if message == state.restart_message {
            state.restarted.set(true);
            PostMessageW(hwnd, WM_NULL, 0, 0);
        } else if message == CALLBACK {
            let event = lparam as u32 & 0xffff;
            if matches!(event, NIN_SELECT | NIN_KEYSELECT | WM_CONTEXTMENU) {
                state.clicks.borrow_mut().push(Click {
                    id: ((lparam as u32 >> 16) & 0xffff) as u16,
                    primary: event != WM_CONTEXTMENU,
                    point: (
                        wparam as u16 as i16 as i32,
                        (wparam >> 16) as u16 as i16 as i32,
                    ),
                });
                PostMessageW(hwnd, WM_NULL, 0, 0);
            }
            return 0;
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

#[cfg(test)]
mod tests;
