//! Raycast-style secondary action panel (`Ctrl+K` / Actions chip).

use crate::providers::ActionSpec;
use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, Button, Label, Orientation, Popover, PositionType, Widget,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

type ActivateCb = Rc<RefCell<Option<Rc<dyn Fn(ActionSpec)>>>>;

pub(crate) struct ActionPanel {
    popover: Popover,
    /// Vertical stack of action buttons (not ListBox — row-activate is flaky
    /// inside Popover under layer-shell; buttons always get real clicks).
    list: GtkBox,
    items: Rc<RefCell<Vec<ActionSpec>>>,
    selected: Rc<Cell<usize>>,
    open: Rc<Cell<bool>>,
    /// Prevents double-fire if a handler is invoked twice.
    firing: Rc<Cell<bool>>,
    on_activate: ActivateCb,
    /// Button widgets for selection highlight / keyboard nav.
    buttons: Rc<RefCell<Vec<Button>>>,
}

impl ActionPanel {
    pub fn new(parent: &impl IsA<Widget>) -> Rc<Self> {
        let popover = Popover::new();
        popover.set_parent(parent);
        popover.set_position(PositionType::Top);
        popover.set_autohide(true);
        popover.set_has_arrow(false);
        popover.add_css_class("blink-action-panel");

        let outer = GtkBox::new(Orientation::Vertical, 4);
        outer.add_css_class("blink-action-panel-inner");
        outer.set_margin_top(6);
        outer.set_margin_bottom(6);
        outer.set_margin_start(6);
        outer.set_margin_end(6);

        let header = Label::new(Some("Actions"));
        header.add_css_class("blink-action-panel-header");
        header.set_halign(Align::Start);
        header.set_margin_start(6);
        header.set_margin_bottom(2);
        header.set_can_target(false);

        let list = GtkBox::new(Orientation::Vertical, 2);
        list.add_css_class("blink-action-panel-list");

        outer.append(&header);
        outer.append(&list);
        popover.set_child(Some(&outer));

        let items = Rc::new(RefCell::new(Vec::new()));
        let selected = Rc::new(Cell::new(0));
        let open = Rc::new(Cell::new(false));
        let firing = Rc::new(Cell::new(false));
        let on_activate: ActivateCb = Rc::new(RefCell::new(None));
        let buttons = Rc::new(RefCell::new(Vec::new()));

        {
            let open = open.clone();
            let firing = firing.clone();
            popover.connect_closed(move |_| {
                open.set(false);
                firing.set(false);
            });
        }

        Rc::new(Self {
            popover,
            list,
            items,
            selected,
            open,
            firing,
            on_activate,
            buttons,
        })
    }

    pub fn set_on_activate(&self, cb: Rc<dyn Fn(ActionSpec)>) {
        *self.on_activate.borrow_mut() = Some(cb);
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    pub fn close(&self) {
        if self.open.get() || self.popover.is_visible() {
            self.popover.popdown();
            self.open.set(false);
        }
    }

    /// Populate and show. Returns false when `specs` is empty.
    pub fn open_for(&self, specs: Vec<ActionSpec>) -> bool {
        if specs.is_empty() {
            self.close();
            return false;
        }

        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.buttons.borrow_mut().clear();

        for (idx, spec) in specs.iter().enumerate() {
            let btn = Button::new();
            btn.add_css_class("blink-action-panel-row");
            btn.add_css_class("flat");
            btn.set_halign(Align::Fill);
            btn.set_hexpand(true);
            btn.set_focus_on_click(false);
            if spec.destructive {
                btn.add_css_class("destructive");
            }

            let line = GtkBox::new(Orientation::Horizontal, 12);
            line.set_margin_top(4);
            line.set_margin_bottom(4);
            line.set_margin_start(4);
            line.set_margin_end(4);
            line.set_can_target(false);

            let label = Label::new(Some(&spec.label));
            label.add_css_class("blink-action-panel-label");
            label.set_halign(Align::Start);
            label.set_hexpand(true);
            label.set_can_target(false);
            if spec.destructive {
                label.add_css_class("destructive");
            }
            line.append(&label);

            if let Some(keys) = spec.shortcut {
                let hint = Label::new(Some(keys));
                hint.add_css_class("blink-action-panel-shortcut");
                hint.set_halign(Align::End);
                hint.set_can_target(false);
                line.append(&hint);
            }

            btn.set_child(Some(&line));

            {
                let items = self.items.clone();
                let popover = self.popover.clone();
                let open = self.open.clone();
                let firing = self.firing.clone();
                let on_activate = self.on_activate.clone();
                let selected = self.selected.clone();
                let buttons = self.buttons.clone();
                btn.connect_clicked(move |_| {
                    selected.set(idx);
                    paint_selection(&buttons, idx);
                    fire_activate(idx, &items, &open, &firing, &popover, &on_activate);
                });
            }

            self.list.append(&btn);
            self.buttons.borrow_mut().push(btn);
        }

        *self.items.borrow_mut() = specs;
        self.selected.set(0);
        self.firing.set(false);
        paint_selection(&self.buttons, 0);

        self.open.set(true);
        self.popover.popup();
        // Focus first button so Enter works without re-selecting.
        if let Some(btn) = self.buttons.borrow().first() {
            btn.grab_focus();
        }
        true
    }

    pub fn move_selection(&self, delta: i32) {
        let n = self.items.borrow().len();
        if n == 0 {
            return;
        }
        let cur = self.selected.get();
        let next = if delta > 0 {
            (cur + 1) % n
        } else if cur == 0 {
            n - 1
        } else {
            cur - 1
        };
        self.selected.set(next);
        paint_selection(&self.buttons, next);
        if let Some(btn) = self.buttons.borrow().get(next) {
            btn.grab_focus();
        }
    }

    pub fn activate_selected(&self) -> Option<ActionSpec> {
        let idx = self.selected.get();
        let spec = self.items.borrow().get(idx).cloned();
        if spec.is_some() {
            self.close();
        }
        spec
    }
}

fn paint_selection(buttons: &Rc<RefCell<Vec<Button>>>, selected: usize) {
    for (i, btn) in buttons.borrow().iter().enumerate() {
        if i == selected {
            btn.add_css_class("selected");
        } else {
            btn.remove_css_class("selected");
        }
    }
}

fn fire_activate(
    idx: usize,
    items: &Rc<RefCell<Vec<ActionSpec>>>,
    open: &Rc<Cell<bool>>,
    firing: &Rc<Cell<bool>>,
    popover: &Popover,
    on_activate: &ActivateCb,
) {
    if firing.get() {
        return;
    }
    let Some(spec) = items.borrow().get(idx).cloned() else {
        return;
    };
    let Some(cb) = on_activate.borrow().clone() else {
        eprintln!("blink: action panel activate with no callback");
        return;
    };
    firing.set(true);
    open.set(false);
    // Close the popover *after* running the action. Deferring via idle let
    // layer-shell focus-loss hide the window and swallow the click path
    // (keyboard shortcuts still worked because they never used this path).
    cb(spec);
    popover.popdown();
}
