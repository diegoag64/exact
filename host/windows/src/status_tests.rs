use exact_kernel::{HostCover, NodeKey};
use exact_painted::{
    presenter::status_items::{Entry, Item},
    Presenter,
};

pub(crate) const APP: &str = r#"component App
  state checked = false
  state visible = true
  state locked = false
  action toggle
    checked = !checked
  action disable
    locked = true
  action remove
    visible = false
  action quit
    close()
  view
    column
      if visible
        button "Ember" -exact-menu="status" popovertarget="app" press=toggle disabled=locked testId="status"
      column id="app" popover="auto" role="menu"
        button "Start at Login" role="menuitemcheckbox" aria-checked=checked press=toggle testId="login"
        button "Unavailable" disabled=true press=toggle testId="disabled"
        hr
        button "Quit Ember" press=quit testId="quit"
      button "Disable" press=disable testId="disable"
      button "Remove" press=remove testId="remove"
"#;

pub(crate) fn boot() -> Presenter<()> {
    let plan = contract::compile(APP).unwrap().encode();
    let (mut p, error) = Presenter::boot_with(
        &plan,
        (),
        (400., 300.),
        1.,
        Default::default(),
        exact_painted::presenter::PainterChoice::Cpu,
    )
    .unwrap();
    assert!(error.is_none(), "{error:?}");
    p.serve_overlays();
    p.frame();
    p
}

pub(crate) fn key(p: &Presenter<()>, name: &str) -> NodeKey {
    p.host().kernel().find_by_test_id(name)[0]
}

pub(crate) fn checked(item: &Item) -> bool {
    matches!(&item.menu[0], Entry::Item { row, .. } if row.checked)
}

#[test]
fn status_projection_replaces_the_button_and_validates_live_actions() {
    crate::on_desktop_stack(|| {
        let mut p = boot();
        let source = key(&p, "status");
        let login = key(&p, "login");
        let disabled = key(&p, "disabled");
        let quit = key(&p, "quit");
        assert!(!checked(&p.status_items()[0]));
        p.place_status_items(vec![source]);
        let k = p.host().kernel();
        assert_eq!(k.arena().cover(source.index), Some(HostCover::Whole));
        let id = k.node_by_key(source).unwrap().id;
        assert!(!p.access_tree().0.iter().any(|n| n.view == id));
        assert!(p.activate_status_item(source, Some(login)));
        assert!(checked(&p.status_items()[0]));
        assert!(!p.activate_status_item(source, Some(disabled)));
        assert!(checked(&p.status_items()[0]));
        assert!(p.activate_status_item(source, None));
        assert!(!checked(&p.status_items()[0]));
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let count = closed.clone();
        p.set_window_closer(move || count.set(true));
        assert!(p.activate_status_item(source, Some(quit)));
        p.run_commands(|| ());
        assert!(closed.get());
        p.place_status_items(Vec::new());
        assert_eq!(p.host().kernel().arena().cover(source.index), None);
        assert!(p.access_tree().0.iter().any(|n| n.view == id));
        let disable = p
            .host()
            .kernel()
            .node_by_key(key(&p, "disable"))
            .unwrap()
            .id;
        p.dispatch_press(disable, 0., false);
        assert!(!p.activate_status_item(source, Some(login)));
        let remove = p.host().kernel().node_by_key(key(&p, "remove")).unwrap().id;
        p.dispatch_press(remove, 0., false);
        assert!(p.status_items().is_empty());
        assert!(!p.activate_status_item(source, Some(quit)));
    });
}
