//! The Windows notification area's buttons and menus, read from the live tree.
use super::*;
use exact_kernel::{HostCover, NodeKey};
use top::shape;

/// A status button's current presentation. Keys fence callbacks after removal.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// The authored button.
    pub key: NodeKey,
    /// Its accessible label, title, or text.
    pub title: String,
    /// Whether activation is allowed.
    pub enabled: bool,
    /// A primary click runs this button's own action when present.
    pub press: bool,
    /// Its menu-shaped popover, including submenus.
    pub menu: Vec<Entry>,
}

/// A row of a native status menu.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// A selectable button and its optional submenu.
    Item {
        /// The button's stable identity.
        key: NodeKey,
        /// The shared menu shape's label and state.
        row: top::Row,
        /// The menu this button opens.
        children: Vec<Entry>,
    },
    /// An authored separator or section boundary.
    Separator,
    /// A section's nonselectable label.
    Header(String),
}

impl<D: DataSource> Presenter<D> {
    /// Status buttons that are mounted and authored as visible, even in a hidden window.
    pub fn status_items(&self) -> Vec<Item> {
        let runner = self.host.runner();
        let k = self.host.kernel();
        exact_runner::desktop::buttons(runner, "status")
            .into_iter()
            .filter(|id| !self.host.route_visibility(*id).0 && crate::paint::revealed(k, *id, 0))
            .filter_map(|id| {
                let n = k.node(id)?;
                let title = n
                    .props
                    .str(PropId::AccessibilityLabel)
                    .or_else(|| n.props.str(PropId::Title))
                    .map(str::to_owned)
                    .unwrap_or_else(|| exact_runner::desktop::label(k, id));
                let menu = exact_kernel::Invocation::of(k, &n)
                    .filter(|i| i.opens())
                    .map_or_else(Vec::new, |i| self.status_menu(i.target, &mut Vec::new()));
                Some(Item {
                    key: n.key,
                    title,
                    enabled: n.props.bool(PropId::Disabled) != Some(true),
                    press: runner.handlers_of(id).contains(&EventKind::Press),
                    menu,
                })
            })
            .collect()
    }

    fn status_menu(&self, key: NodeKey, visited: &mut Vec<NodeKey>) -> Vec<Entry> {
        if visited.contains(&key) {
            return Vec::new();
        }
        let k = self.host.kernel();
        let Some(pop) = k.node_by_key(key) else {
            return Vec::new();
        };
        let pressed = |id| {
            self.host
                .runner()
                .handlers_of(id)
                .contains(&EventKind::Press)
        };
        let Some(top::Plan::Menu(entries)) = shape::menu(k, pop.id, &pressed) else {
            return Vec::new();
        };
        visited.push(key);
        let result = entries
            .into_iter()
            .filter_map(|entry| {
                Some(match entry {
                    top::Entry::Separator => Entry::Separator,
                    top::Entry::Header(title, _, _) => Entry::Header(title),
                    top::Entry::Item(row) => {
                        let n = k.node(row.node)?;
                        let mut at = Some(n.id);
                        let mut displayed = true;
                        while let Some(id) = at {
                            let Some(ancestor) = k.node(id) else {
                                break;
                            };
                            displayed &= ancestor.style.display != exact_kernel::Display::None;
                            at = ancestor.parent;
                        }
                        if !displayed || !crate::paint::revealed(k, n.id, 0) {
                            return None;
                        }
                        let children = row
                            .opens
                            .map_or_else(Vec::new, |key| self.status_menu(key, visited));
                        Entry::Item {
                            key: n.key,
                            row,
                            children,
                        }
                    }
                })
            })
            .collect();
        visited.pop();
        result
    }

    /// Replace page buttons only after the carrier has registered their icons.
    pub fn place_status_items(&mut self, placed: Vec<NodeKey>) {
        let before = std::mem::replace(&mut self.window.status_items, placed);
        let k = self.host.runner_mut().kernel_mut();
        let mut changed = false;
        for key in before.iter().chain(&self.window.status_items) {
            let Some(n) = k.node_by_key(*key) else {
                continue;
            };
            let id = n.id;
            let cover = self
                .window
                .status_items
                .contains(key)
                .then_some(HostCover::Whole);
            if k.arena().slot_of(id).and_then(|s| k.arena().cover(s)) != cover {
                changed |= k.set_host_cover(id, cover).is_ok();
            }
        }
        if changed {
            self.dirty = true;
            if let Err(e) = self.host.layout() {
                self.host.log(e);
            }
            if let Some(e) = self.after_commit() {
                self.host.log(e);
            }
        }
    }

    /// Run a native activation only if the source and chosen row are still enabled.
    /// `None` chooses the status button's own `press`, without opening its popover.
    pub fn activate_status_item(&mut self, source: NodeKey, chosen: Option<NodeKey>) -> bool {
        let Some(item) = self
            .status_items()
            .into_iter()
            .find(|i| i.key == source && i.enabled)
        else {
            return false;
        };
        fn allows(entries: &[Entry], key: NodeKey) -> bool {
            entries.iter().any(|entry| match entry {
                Entry::Item {
                    key: own,
                    row,
                    children,
                } if row.enabled => (*own == key && children.is_empty()) || allows(children, key),
                _ => false,
            })
        }
        if !chosen.map_or(item.press, |key| allows(&item.menu, key)) {
            return false;
        }
        let key = chosen.unwrap_or(source);
        let Some(id) = self.host.kernel().node_by_key(key).map(|n| n.id) else {
            return false;
        };
        let now = self.host.now();
        if chosen.is_some() {
            self.activate_with(id, now, false);
        } else {
            self.dispatch_press(id, now, false);
        }
        true
    }
}
