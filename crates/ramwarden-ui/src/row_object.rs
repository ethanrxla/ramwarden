//! A `GObject` wrapper so rows can live in a `ColumnView`.
//!
//! `ColumnView` needs a `ListModel` of `GObject`s. The data itself stays a plain
//! [`crate::model::Row`] inside a `RefCell`; this is only the shell that makes it
//! addressable from GTK. No GObject properties are declared — the cell factories
//! downcast and read the row directly, which avoids a large amount of property
//! boilerplate for no benefit.

use std::cell::RefCell;

use gtk4::glib;
use gtk4::subclass::prelude::*;

use crate::model::Row;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RowObject {
        pub row: RefCell<Row>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowObject {
        const NAME: &'static str = "RamWardenRow";
        type Type = super::RowObject;
    }

    impl ObjectImpl for RowObject {}
}

glib::wrapper! {
    pub struct RowObject(ObjectSubclass<imp::RowObject>);
}

impl RowObject {
    pub fn new(row: Row) -> Self {
        let obj: Self = glib::Object::builder().build();
        obj.imp().row.replace(row);
        obj
    }

    /// A clone of the row. Cloning rather than lending a borrow keeps GTK
    /// callbacks from holding a `RefCell` borrow across a signal emission, which
    /// is how these panic at runtime.
    pub fn row(&self) -> Row {
        self.imp().row.borrow().clone()
    }

    pub fn set_selected(&self, selected: bool) {
        self.imp().row.borrow_mut().selected = selected;
    }

    pub fn pid(&self) -> i32 {
        self.imp().row.borrow().pid
    }
}
