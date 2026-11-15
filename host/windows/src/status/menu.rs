//! A notification icon's context menu. Windows owns dismissal, keys and accessibility.
use super::*;
use exact_painted::presenter::status_items::Entry;

pub(super) struct Menu(pub(super) HMENU);
impl Drop for Menu {
    fn drop(&mut self) {
        // SAFETY: the root menu owns every attached submenu.
        unsafe {
            DestroyMenu(self.0);
        }
    }
}

pub(super) fn build(entries: &[Entry], actions: &mut Vec<NodeKey>) -> Result<Menu, String> {
    // SAFETY: handles are owned here; AppendMenu copies labels and takes attached submenus.
    unsafe {
        let menu = Menu(CreatePopupMenu());
        if menu.0.is_null() {
            return Err(error("create menu"));
        }
        for entry in entries {
            let (flags, id, label, submenu) = match entry {
                Entry::Separator => (MF_SEPARATOR, 0, String::new(), None),
                Entry::Header(title) => (MF_STRING | MF_GRAYED, 0, title.clone(), None),
                Entry::Item { key, row, children } => {
                    let mut flags = MF_STRING | if row.enabled { MF_ENABLED } else { MF_GRAYED };
                    if row.checked {
                        flags |= MF_CHECKED;
                    }
                    if children.is_empty() {
                        actions.push(*key);
                        (flags, actions.len(), row.title.clone(), None)
                    } else {
                        let sub = build(children, actions)?;
                        (
                            flags | MF_POPUP,
                            sub.0 as usize,
                            row.title.clone(),
                            Some(sub),
                        )
                    }
                }
            };
            // Author text is literal: '&' must not become a Windows mnemonic.
            let label: Vec<u16> = label
                .replace('&', "&&")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            if AppendMenuW(menu.0, flags, id, label.as_ptr()) == 0 {
                return Err(error("append menu row"));
            }
            if let Some(sub) = submenu {
                std::mem::forget(sub);
            }
        }
        Ok(menu)
    }
}

pub(super) fn show(
    hwnd: HWND,
    entries: &[Entry],
    point: (i32, i32),
) -> Result<Option<NodeKey>, String> {
    let mut actions = Vec::new();
    let menu = build(entries, &mut actions)?;
    // Foreground ownership and a queued message after dismissal are required for
    // tray menus, including hidden owner windows:
    // https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-trackpopupmenuex
    // SAFETY: the HWND and HMENU remain live throughout the modal menu loop.
    let selected = unsafe {
        SetForegroundWindow(hwnd);
        let selected = TrackPopupMenuEx(
            menu.0,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            point.0,
            point.1,
            hwnd,
            ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        selected
    };
    Ok(selected
        .checked_sub(1)
        .and_then(|id| actions.get(id as usize))
        .copied())
}
