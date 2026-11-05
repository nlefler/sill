//! The Mac's menus on the device (kinds 24, 25 and 27; MacMenu.swift, docs/menu-bar-plan.md): a
//! subscription at the first window list, one menu fetched when it opens, a leaf pressed on the Mac.
//! Ids are paths within the host's version; a request carries the title the device showed; tokens
//! strictly increase per connection and come back in `answering`. Pure, time injected.

use crate::clipboard::admits;
use sill_proto::{FetchMenu, MacMenu, MacMenuItem, PressMenuItem};
use std::collections::HashMap;

/// The host takes 20 fetches and 4 presses a second per connection.
pub const FETCHES_PER_SECOND: usize = 20;
pub const PRESSES_PER_SECOND: usize = 4;

#[derive(Clone, Debug, PartialEq)]
enum Pending {
    Fetch(String),
    Press,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuView {
    pub app: String,
    pub top: Vec<MacMenuItem>,
    pub submenus: HashMap<String, Vec<MacMenuItem>>,
    /// "The menus changed..." and the like, until the next answer.
    pub note: Option<String>,
}

#[derive(Debug, Default)]
pub struct MenuState {
    subscribed: bool,
    token: i64,
    version: Option<i64>,
    view: Option<MenuView>,
    pending: HashMap<i64, Pending>,
    fetches: Vec<f64>,
    presses: Vec<f64>,
}

impl MenuState {
    pub fn new() -> MenuState {
        MenuState::default()
    }

    /// The Menus button shows once a top level has arrived (an older host never sends one).
    pub fn view(&self) -> Option<&MenuView> {
        self.view.as_ref().filter(|v| !v.top.is_empty())
    }

    /// The kind 27 without an id, once per connection, after the first window list.
    pub fn subscribe(&mut self) -> Option<FetchMenu> {
        if self.subscribed {
            return None;
        }
        self.subscribed = true;
        Some(FetchMenu::default())
    }

    fn next_token(&mut self) -> i64 {
        self.token += 1;
        self.token
    }

    /// A menu is opened: fetch its items under the version and title shown. Nothing without a top
    /// level, for a separator or a leaf, or past the rate.
    pub fn open(&mut self, now: f64, id: &str, title: &str) -> Option<FetchMenu> {
        let version = self.version?;
        let view = self.view.as_ref()?;
        let item = find(view, id)?;
        if item.submenu != Some(true) || item.enabled == Some(false) || item.title.as_deref() != Some(title) {
            return None;
        }
        if self.pending.values().any(|p| *p == Pending::Fetch(id.to_string())) {
            return None;
        }
        let (ok, kept) = rate(&self.fetches, now, FETCHES_PER_SECOND);
        self.fetches = kept;
        if !ok {
            return None;
        }
        let token = self.next_token();
        self.pending.insert(token, Pending::Fetch(id.to_string()));
        Some(FetchMenu { version: Some(version), id: Some(id.to_string()), title: Some(title.to_string()), token: Some(token) })
    }

    /// A leaf is chosen. Only an enabled leaf the device was shown, under the title shown.
    pub fn press(&mut self, now: f64, id: &str, title: &str) -> Option<PressMenuItem> {
        let version = self.version?;
        let view = self.view.as_ref()?;
        let item = find(view, id)?;
        if item.separator == Some(true) || item.submenu == Some(true) || item.enabled == Some(false) || item.title.as_deref() != Some(title) {
            return None;
        }
        let (ok, kept) = rate(&self.presses, now, PRESSES_PER_SECOND);
        self.presses = kept;
        if !ok {
            return None;
        }
        let token = self.next_token();
        self.pending.insert(token, Pending::Press);
        Some(PressMenuItem { version: Some(version), id: Some(id.to_string()), title: Some(title.to_string()), token: Some(token) })
    }

    /// Kind 24. Returns `true` when what the window shows changed.
    pub fn on_menu(&mut self, m: &MacMenu) -> bool {
        let mut changed = false;
        if let Some(top) = &m.menus {
            let moved = self.version != m.version;
            if moved {
                // Another version: every id of the old one means nothing now.
                self.pending.clear();
            }
            self.version = m.version;
            let mut view = self.view.take().unwrap_or_default();
            if moved || view.top != *top || view.app != m.app.clone().unwrap_or_default() {
                view.submenus.clear();
            }
            view.app = m.app.clone().unwrap_or_default();
            view.top = top.clone();
            view.note = None;
            self.view = Some(view);
            return true;
        }
        let answered = m.answering.and_then(|t| self.pending.remove(&t));
        let was_answer = answered.is_some();
        match answered {
            Some(Pending::Fetch(id)) => {
                if let (Some(view), Some(items)) = (self.view.as_mut(), m.items.as_ref()) {
                    if m.stale != Some(true) && m.menu.as_deref().is_none_or(|x| x == id) {
                        view.submenus.insert(id, items.clone());
                        view.note = None;
                        changed = true;
                    }
                }
            }
            Some(Pending::Press) if m.pressed != Some(true) => {
                changed |= self.note(m.note.clone().unwrap_or_else(|| "The Mac did not choose that item.".into()));
                return changed;
            }
            Some(Pending::Press) => {}
            None => {}
        }
        if m.stale == Some(true) || (m.pressed == Some(false) && !was_answer) {
            if let Some(n) = &m.note {
                changed |= self.note(n.clone());
            }
            // The cached submenus are of an old version.
            if let Some(v) = self.view.as_mut() {
                if !v.submenus.is_empty() {
                    v.submenus.clear();
                    changed = true;
                }
            }
        }
        changed
    }

    fn note(&mut self, text: String) -> bool {
        match self.view.as_mut() {
            Some(v) if v.note.as_deref() != Some(&text) => {
                v.note = Some(text);
                true
            }
            _ => false,
        }
    }
}

fn rate(arrivals: &[f64], now: f64, per_second: usize) -> (bool, Vec<f64>) {
    if per_second == crate::clipboard::PER_SECOND {
        return admits(arrivals, now);
    }
    let mut recent: Vec<f64> = arrivals.iter().copied().filter(|t| now - t < 1.0).collect();
    if recent.len() >= per_second {
        return (false, recent);
    }
    recent.push(now);
    (true, recent)
}

/// The item an id names in what is shown: ids are child-index paths ("2.9") and the host numbers them,
/// so look it up by the id in the top level or in a cached submenu.
fn find<'a>(v: &'a MenuView, id: &str) -> Option<&'a MacMenuItem> {
    v.top.iter().chain(v.submenus.values().flatten()).find(|i| i.id.as_deref() == Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, title: &str) -> MacMenuItem {
        MacMenuItem { id: Some(id.into()), title: Some(title.into()), ..Default::default() }
    }
    fn sub(id: &str, title: &str) -> MacMenuItem {
        MacMenuItem { submenu: Some(true), ..item(id, title) }
    }
    fn top(version: i64) -> MacMenu {
        MacMenu { version: Some(version), app: Some("Notes".into()), menus: Some(vec![sub("1", "File"), sub("2", "Edit")]), ..Default::default() }
    }
    fn answer(token: i64, menu: &str, items: Vec<MacMenuItem>) -> MacMenu {
        MacMenu { answering: Some(token), menu: Some(menu.into()), items: Some(items), ..Default::default() }
    }

    #[test]
    fn subscribes_once() {
        let mut s = MenuState::new();
        assert_eq!(s.subscribe(), Some(FetchMenu::default()));
        assert!(s.subscribe().is_none());
        assert!(s.view().is_none(), "no button before a top level arrives");
    }

    #[test]
    fn open_fetches_under_the_shown_title_and_the_answer_fills_the_menu() {
        let mut s = MenuState::new();
        assert!(s.open(0.0, "1", "File").is_none(), "no top level yet");
        assert!(s.on_menu(&top(5)));
        assert_eq!(s.view().unwrap().app, "Notes");
        let f = s.open(0.0, "1", "File").unwrap();
        assert_eq!((f.version, f.id.as_deref(), f.title.as_deref(), f.token), (Some(5), Some("1"), Some("File"), Some(1)));
        assert!(s.open(0.1, "1", "File").is_none(), "already asked");
        assert!(s.open(0.1, "1", "Files").is_none(), "not the title shown");
        assert!(s.open(0.1, "9", "Nope").is_none());
        assert!(s.on_menu(&answer(1, "1", vec![item("1.0", "New"), item("1.1", "Open")])));
        assert_eq!(s.view().unwrap().submenus["1"].len(), 2);
        // Opened again: fetched again (the cache shows meanwhile).
        let g = s.open(1.0, "1", "File").unwrap();
        assert_eq!(g.token, Some(2), "tokens only rise");
    }

    #[test]
    fn press_only_an_enabled_leaf_that_was_shown() {
        let mut s = MenuState::new();
        s.on_menu(&top(5));
        s.open(0.0, "1", "File");
        let mut disabled = item("1.1", "Save");
        disabled.enabled = Some(false);
        let sep = MacMenuItem { id: Some("1.2".into()), separator: Some(true), ..Default::default() };
        s.on_menu(&answer(1, "1", vec![item("1.0", "New"), disabled, sep, sub("1.3", "Open Recent")]));
        assert!(s.press(1.0, "1.1", "Save").is_none(), "disabled");
        assert!(s.press(1.0, "1.2", "").is_none(), "separator");
        assert!(s.press(1.0, "1.3", "Open Recent").is_none(), "a submenu is opened, not pressed");
        assert!(s.press(1.0, "1.0", "Newer").is_none(), "title changed");
        assert!(s.press(1.0, "7.7", "New").is_none(), "never shown");
        let p = s.press(1.0, "1.0", "New").unwrap();
        assert_eq!((p.version, p.id.as_deref(), p.title.as_deref()), (Some(5), Some("1.0"), Some("New")));
    }

    #[test]
    fn a_new_version_drops_the_old_ids() {
        let mut s = MenuState::new();
        s.on_menu(&top(5));
        s.open(0.0, "1", "File");
        s.on_menu(&answer(1, "1", vec![item("1.0", "New")]));
        assert!(s.on_menu(&top(6)));
        assert!(s.view().unwrap().submenus.is_empty());
        assert!(s.press(1.0, "1.0", "New").is_none());
        // A late answer for a token of version 5 is ignored.
        s.open(0.0, "2", "Edit");
        s.on_menu(&top(7));
        assert!(!s.on_menu(&answer(2, "2", vec![item("2.0", "Undo")])));
        assert!(s.view().unwrap().submenus.is_empty());
    }

    #[test]
    fn a_stale_answer_says_so_and_clears_the_cache() {
        let mut s = MenuState::new();
        s.on_menu(&top(5));
        s.open(0.0, "1", "File");
        s.on_menu(&answer(1, "1", vec![item("1.0", "New")]));
        s.open(0.1, "2", "Edit");
        let stale = MacMenu { answering: Some(2), stale: Some(true), note: Some("The menus changed. Open the menu again.".into()), ..Default::default() };
        assert!(s.on_menu(&stale));
        let v = s.view().unwrap();
        assert!(v.submenus.is_empty());
        assert_eq!(v.note.as_deref(), Some("The menus changed. Open the menu again."));
        s.on_menu(&top(6));
        assert!(s.view().unwrap().note.is_none(), "the next top level clears it");
    }

    #[test]
    fn a_refused_press_leaves_a_note() {
        let mut s = MenuState::new();
        s.on_menu(&top(5));
        s.open(0.0, "1", "File");
        s.on_menu(&answer(1, "1", vec![item("1.0", "New")]));
        s.press(1.0, "1.0", "New");
        let refused = MacMenu { answering: Some(2), pressed: Some(false), note: Some("That item is not available.".into()), ..Default::default() };
        assert!(s.on_menu(&refused));
        assert_eq!(s.view().unwrap().note.as_deref(), Some("That item is not available."));
        s.press(2.0, "1.0", "New");
        assert!(!s.on_menu(&MacMenu { answering: Some(3), pressed: Some(true), ..Default::default() }));
    }

    #[test]
    fn presses_are_rated_four_a_second() {
        let mut s = MenuState::new();
        s.on_menu(&top(5));
        s.open(0.0, "1", "File");
        s.on_menu(&answer(1, "1", vec![item("1.0", "New")]));
        let n = (0..6).filter(|i| s.press(1.0 + *i as f64 * 0.01, "1.0", "New").is_some()).count();
        assert_eq!(n, 4);
        assert!(s.press(2.1, "1.0", "New").is_some());
    }
}
