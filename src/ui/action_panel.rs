//! Raycast-style secondary action panel (`Ctrl+K` / Actions chip).

use crate::providers::ActionSpec;
use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, Label, ListBox, ListBoxRow, Orientation, Popover, PositionType,
    Widget,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

type ActivateCb = Rc<RefCell<Option<Rc<dyn Fn(ActionSpec)>>>>;

pub(crate) struct ActionPanel {
    popover: Popover,
    list: ListBox,
    items: Rc<RefCell<Vec<ActionSpec>>>,
    selected: Rc<Cell<usize>>,
    open: Rc<Cell<bool>>,
    on_activate: ActivateCb,
}

impl ActionPanel {
    pub fn new(parent: &impl IsA<Widget>) -> Rc<Self> {
        let popover = Popover::new();
        popover.set_parent(parent);
        popover.set_position(PositionType::Top);
        popover.set_autohide(true);
        popover.set_has_arrow(true);
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

        let list = ListBox::new();
        list.add_css_class("blink-action-panel-list");
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);

        outer.append(&header);
        outer.append(&list);
        popover.set_child(Some(&outer));

        let items = Rc::new(RefCell::new(Vec::new()));
        let selected = Rc::new(Cell::new(0));
        let open = Rc::new(Cell::new(false));
        let on_activate: ActivateCb = Rc::new(RefCell::new(None));

        {
            let open = open.clone();
            popover.connect_closed(move |_| {
                open.set(false);
            });
        }

        {
            let selected = selected.clone();
            list.connect_row_selected(move |_, row| {
                if let Some(row) = row {
                    selected.set(row.index() as usize);
                }
            });
        }

        {
            let items = items.clone();
            let popover = popover.clone();
            let open = open.clone();
            let on_activate = on_activate.clone();
            list.connect_row_activated(move |_, row| {
                let idx = row.index() as usize;
                let spec = items.borrow().get(idx).cloned();
                open.set(false);
                popover.popdown();
                if let Some(spec) = spec {
                    if let Some(cb) = on_activate.borrow().clone() {
                        cb(spec);
                    }
                }
            });
        }

        Rc::new(Self {
            popover,
            list,
            items,
            selected,
            open,
            on_activate,
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

        for spec in &specs {
            let row = ListBoxRow::new();
            row.add_css_class("blink-action-panel-row");
            if spec.destructive {
                row.add_css_class("destructive");
            }
            row.set_activatable(true);

            let line = GtkBox::new(Orientation::Horizontal, 12);
            line.set_margin_top(6);
            line.set_margin_bottom(6);
            line.set_margin_start(8);
            line.set_margin_end(8);

            let label = Label::new(Some(&spec.label));
            label.add_css_class("blink-action-panel-label");
            label.set_halign(Align::Start);
            label.set_hexpand(true);
            if spec.destructive {
                label.add_css_class("destructive");
            }
            line.append(&label);

            if let Some(keys) = spec.shortcut {
                let hint = Label::new(Some(keys));
                hint.add_css_class("blink-action-panel-shortcut");
                hint.set_halign(Align::End);
                line.append(&hint);
            }

            row.set_child(Some(&line));
            self.list.append(&row);
        }

        *self.items.borrow_mut() = specs;
        self.selected.set(0);
        if let Some(row) = self.list.row_at_index(0) {
            self.list.select_row(Some(&row));
        }

        self.open.set(true);
        self.popover.popup();
        // Keep focus on list for ↑/↓/Enter inside the panel.
        self.list.grab_focus();
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
        if let Some(row) = self.list.row_at_index(next as i32) {
            self.list.select_row(Some(&row));
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
