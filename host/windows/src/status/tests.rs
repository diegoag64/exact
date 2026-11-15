use super::*;
use crate::status_tests::{boot, checked, key};

struct Owner(HWND);
impl Owner {
    fn new() -> Self {
        // SAFETY: STATIC is a system class; the invisible window belongs to this thread.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                w!("Exact status test"),
                0,
                0,
                0,
                1,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        assert!(!hwnd.is_null());
        Self(hwnd)
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

#[test]
fn native_menu_has_live_checks_disabled_rows_and_separators() {
    crate::on_desktop_stack(|| {
        let mut p = boot();
        for expected in [false, true] {
            let item = p.status_items().remove(0);
            let mut actions = Vec::new();
            let menu = menu::build(&item.menu, &mut actions).unwrap();
            // SAFETY: inspect a live HMENU made by our builder.
            unsafe {
                assert_eq!(GetMenuItemCount(menu.0), 4);
                assert_eq!(
                    GetMenuState(menu.0, 0, MF_BYPOSITION) & MF_CHECKED != 0,
                    expected
                );
                assert_ne!(GetMenuState(menu.0, 1, MF_BYPOSITION) & MF_GRAYED, 0);
                assert_ne!(GetMenuState(menu.0, 2, MF_BYPOSITION) & MF_SEPARATOR, 0);
            }
            assert_eq!(
                actions,
                vec![key(&p, "login"), key(&p, "disabled"), key(&p, "quit")]
            );
            p.activate_status_item(item.key, Some(actions[0]));
        }
    });
}

#[test]
fn callbacks_decode_primary_secondary_keyboard_and_explorer_restart() {
    let owner = Owner::new();
    let status = Status::new(owner.0 as isize).unwrap();
    for (event, primary) in [
        (NIN_SELECT, true),
        (NIN_KEYSELECT, true),
        (WM_CONTEXTMENU, false),
    ] {
        unsafe {
            SendMessageW(
                owner.0,
                CALLBACK,
                ((20u32 << 16) | (-30i16 as u16 as u32)) as usize,
                ((7u32 << 16) | event) as isize,
            );
        }
        assert_eq!(
            status.messages.clicks.borrow_mut().pop(),
            Some(Click {
                id: 7,
                primary,
                point: (-30, 20)
            })
        );
    }
    unsafe {
        SendMessageW(owner.0, status.messages.restart_message, 0, 0);
    }
    assert!(status.messages.restarted.get());
    drop(owner);
    assert!(!status.messages.alive.get());
    drop(status);
}

#[test]
#[ignore = "requires an interactive Windows session with Explorer"]
fn shell_icon_survives_a_hidden_window_and_recovers_after_taskbar_created() {
    crate::on_desktop_stack(|| {
        let owner = Owner::new();
        let mut p = boot();
        let mut status = Status::new(owner.0 as isize).unwrap();
        status.sync(&mut p);
        assert_eq!(status.items.len(), 1);
        assert!(status.items[0].registered);
        let id = status.items[0].id;
        let identifier = NOTIFYICONIDENTIFIER {
            cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: owner.0,
            uID: id.into(),
            ..Default::default()
        };
        let present = || {
            let mut rect = Default::default();
            unsafe { Shell_NotifyIconGetRect(&identifier, &mut rect) >= 0 }
        };
        assert!(present());
        assert_eq!(unsafe { IsWindowVisible(owner.0) }, 0);
        unsafe {
            SendMessageW(
                owner.0,
                CALLBACK,
                0,
                ((u32::from(id) << 16) | NIN_SELECT) as isize,
            );
        }
        status.service(&mut p, || 0.);
        assert!(checked(&p.status_items()[0]));
        assert_eq!(unsafe { IsWindowVisible(owner.0) }, 0);
        assert!(notify(NIM_DELETE, &data(owner.0, id)));
        assert!(!present());
        unsafe {
            SendMessageW(owner.0, status.messages.restart_message, 0, 0);
        }
        status.sync(&mut p);
        assert!(present());
        drop(status);
        assert!(!present());
    });
}

// End only once the native menu is open and waiting for input.
unsafe extern "system" fn dismiss_ready(
    hwnd: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
    _: usize,
    raw: usize,
) -> isize {
    unsafe {
        if message == WM_ENTERIDLE && wparam == MSGF_MENU as usize {
            let opened = &*(raw as *const Cell<u32>);
            opened.set(opened.get() + 1);
            EndMenu();
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

unsafe extern "system" fn cancel_hung_menu(hwnd: HWND, _: u32, id: usize, _: u32) {
    unsafe {
        KillTimer(hwnd, id);
        EndMenu();
    }
}

#[test]
#[ignore = "opens a native menu in an interactive Windows session"]
fn either_button_opens_the_menu_without_showing_the_owner_window() {
    crate::on_desktop_stack(|| {
        let opened = Cell::new(0u32);
        let owner = Owner::new();
        let mut p = boot();
        let source =
            crate::status_tests::APP.replace("press=toggle disabled=locked", "disabled=locked");
        p.reload(&contract::compile(&source).unwrap().encode(), ())
            .unwrap();
        let mut status = Status::new(owner.0 as isize).unwrap();
        status.sync(&mut p);
        assert!(status.items[0].registered);
        assert!(!p.status_items()[0].press);
        unsafe {
            assert_ne!(
                SetWindowSubclass(
                    owner.0,
                    Some(dismiss_ready),
                    1,
                    &opened as *const Cell<u32> as usize
                ),
                0
            );
        }
        for (event, expected) in [(NIN_SELECT, 1), (WM_CONTEXTMENU, 2)] {
            let id = status.items[0].id;
            let timer = unsafe { SetTimer(owner.0, 1, 10_000, Some(cancel_hung_menu)) };
            assert_ne!(timer, 0);
            unsafe {
                SendMessageW(
                    owner.0,
                    CALLBACK,
                    0,
                    ((u32::from(id) << 16) | event) as isize,
                );
            }
            status.service(&mut p, || 0.);
            unsafe {
                KillTimer(owner.0, timer);
            }
            assert_eq!(opened.get(), expected);
            assert!(!checked(&p.status_items()[0]), "dismissal runs no action");
            assert_eq!(unsafe { IsWindowVisible(owner.0) }, 0);
        }
    });
}
