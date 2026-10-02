//! Build the real window and assert on the widget tree.
//!
//! A window that starts without crashing is not a window that renders correctly.
//! This walks what was actually constructed and checks the properties the user
//! complained about: resizable columns, a horizontal scrollbar, and widths that
//! do not ellipsize.
//!
//! cargo run -p ramwarden-ui --features gui --example inspect -- [port]

use gtk4::prelude::*;
use ramwarden_ui::client::Client;

fn walk(w: &gtk4::Widget, depth: usize, out: &mut Vec<(usize, String, gtk4::Widget)>) {
    out.push((depth, w.type_().name().to_string(), w.clone()));
    let mut child = w.first_child();
    while let Some(c) = child {
        walk(&c, depth + 1, out);
        child = c.next_sibling();
    }
}

fn main() {
    let runtime = ramwarden_ui::runtime::network_runtime().expect("network runtime");
    let _entered = runtime.enter();
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(7824);
    let client = Client::new("127.0.0.1", port).expect("client");

    let app = gtk4::Application::builder()
        .application_id("com.system76.RamWardenInspect")
        .build();

    app.connect_activate(move |app| {
        // Build the real window through the real code path.
        let client = Client::new("127.0.0.1", port).expect("client");
        ramwarden_ui::app::build_for_test(app, client);

        let window = app.windows().into_iter().next().expect("a window");
        let mut tree = Vec::new();
        walk(window.upcast_ref::<gtk4::Widget>(), 0, &mut tree);

        let mut failures: Vec<String> = Vec::new();

        // ── The column view ─────────────────────────────────────────────────
        let view = tree
            .iter()
            .find_map(|(_, _, w)| w.clone().downcast::<gtk4::ColumnView>().ok())
            .expect("no ColumnView was built");

        let columns = view.columns();
        println!("columns: {}", columns.n_items());
        println!("{:<10} {:>10} {:>11} {:>8}", "TITLE", "WIDTH", "RESIZABLE", "EXPAND");
        for i in 0..columns.n_items() {
            let col = columns
                .item(i)
                .and_downcast::<gtk4::ColumnViewColumn>()
                .expect("column");
            let title = col.title().map(|t| t.to_string()).unwrap_or_default();
            let shown = if title.is_empty() { "(tick)".to_string() } else { title.clone() };
            println!(
                "{:<10} {:>10} {:>11} {:>8}",
                shown,
                col.fixed_width(),
                col.is_resizable(),
                col.expands()
            );

            if title.is_empty() {
                continue;
            }
            // The complaint: columns that could not be widened or read.
            if !col.is_resizable() {
                failures.push(format!("{title} is not resizable"));
            }
            if col.fixed_width() < 80 {
                failures.push(format!("{title} is only {}px wide", col.fixed_width()));
            }
        }

        for want in ["Process", "PID", "RAM", "VRAM", "Status", "Scope", "Why"] {
            let found = (0..columns.n_items()).any(|i| {
                columns
                    .item(i)
                    .and_downcast::<gtk4::ColumnViewColumn>()
                    .and_then(|c| c.title())
                    .is_some_and(|t| t == want)
            });
            if !found {
                failures.push(format!("missing column {want}"));
            }
        }

        // ── Horizontal scrolling, the actual fix ────────────────────────────
        let scrolls: Vec<gtk4::ScrolledWindow> = tree
            .iter()
            .filter_map(|(_, _, w)| w.clone().downcast::<gtk4::ScrolledWindow>().ok())
            .collect();
        let table_scroll = scrolls
            .iter()
            .find(|s| {
                s.child()
                    .is_some_and(|c| c.downcast::<gtk4::ColumnView>().is_ok())
            })
            .expect("the table is not in a ScrolledWindow");
        let (h, v) = table_scroll.policy();
        println!("\ntable scroll policy: horizontal={h:?} vertical={v:?}");
        if h == gtk4::PolicyType::Never {
            failures.push("horizontal scrolling is disabled — this is v1's bug".into());
        }

        // ── The other pieces the window promises ───────────────────────────
        let counts = |name: &str| tree.iter().filter(|(_, t, _)| t == name).count();
        println!(
            "widgets: {} buttons, {} labels, {} entries, {} searchentries, {} frames, {} checkbuttons",
            counts("GtkButton"), counts("GtkLabel"), counts("GtkEntry"),
            counts("GtkSearchEntry"), counts("GtkFrame"), counts("GtkCheckButton"),
        );
        if counts("GtkSearchEntry") == 0 {
            failures.push("no filter box".into());
        }
        if counts("GtkProgressBar") == 0 {
            failures.push("no memory bar".into());
        }

        let labels: Vec<String> = tree
            .iter()
            .filter_map(|(_, _, w)| w.clone().downcast::<gtk4::Button>().ok())
            .filter_map(|b| b.label().map(|l| l.to_string()))
            .collect();
        println!("buttons: {labels:?}");
        for want in ["Reclaim", "Suspend", "Kill", "Analyse"] {
            if !labels.iter().any(|l| l == want) {
                failures.push(format!("missing button {want}"));
            }
        }

        // Sorting must actually be wired: a column advertising a sort that does
        // nothing when clicked is worse than one that does not advertise it.
        let mut sortable = 0;
        for i in 0..columns.n_items() {
            let col = columns.item(i).and_downcast::<gtk4::ColumnViewColumn>().unwrap();
            let title = col.title().map(|t| t.to_string()).unwrap_or_default();
            if title.is_empty() {
                continue;
            }
            match col.sorter() {
                Some(_) => sortable += 1,
                None if title == "Why" => {} // a sentence has no useful order
                None => failures.push(format!("{title} has no sorter — clicking it does nothing")),
            }
        }
        println!("columns with a working sorter: {sortable}");

        // And the view must be using a sorted model, or the sorters are inert.
        match view.model().and_downcast::<gtk4::NoSelection>().and_then(|s| s.model()) {
            Some(m) if m.is::<gtk4::SortListModel>() => {
                println!("view model is sorted: yes");
            }
            _ => failures.push("the view is not backed by a SortListModel".into()),
        }
        match view.sorter() {
            Some(_) => println!("view exposes a sorter: yes"),
            None => failures.push("the view has no sorter".into()),
        }

        // The right-click handler.
        let has_secondary = {
            let mut found = false;
            for c in view.observe_controllers().into_iter().flatten() {
                if let Ok(g) = c.downcast::<gtk4::GestureSingle>()
                    && g.button() == gtk4::gdk::BUTTON_SECONDARY
                {
                    found = true;
                }
            }
            found
        };
        println!("right-click gesture on the table: {has_secondary}");
        if !has_secondary {
            failures.push("no secondary-button gesture — right-click would do nothing".into());
        }

        println!();
        if failures.is_empty() {
            println!("ALL CHECKS PASSED");
        } else {
            println!("FAILURES:");
            for f in &failures {
                println!("  - {f}");
            }
        }
        app.quit();
    });

    let _ = client;
    app.run_with_args::<&str>(&[]);
}
