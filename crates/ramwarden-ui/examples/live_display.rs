//! Read-only smoke check of the real window against a running daemon.
//! Run under Xvfb: cargo run -p ramwarden-ui --features gui --example live_display -- 7823
use gtk4::{gio, glib, prelude::*};
use ramwarden_ui::client::Client;

fn widgets(root: &gtk4::Widget) -> Vec<gtk4::Widget> {
    let mut out = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(w) = child {
        out.extend(widgets(&w));
        child = w.next_sibling();
    }
    out
}

fn main() {
    let port = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(7823);
    let runtime = ramwarden_ui::runtime::network_runtime().unwrap();
    let client = Client::new("127.0.0.1", port).unwrap();
    runtime.block_on(client.verify()).unwrap();
    let _entered = runtime.enter();
    gtk4::init().unwrap();
    let app = gtk4::Application::builder().application_id("com.system76.RamWardenLiveDisplay").build();
    app.register(None::<&gio::Cancellable>).unwrap();
    ramwarden_ui::app::build_for_test(&app, client);
    let window = app.windows()[0].clone();
    let tree = widgets(window.upcast_ref());
    let view = tree.iter().find_map(|w| w.clone().downcast::<gtk4::ColumnView>().ok()).unwrap();
    glib::MainContext::default().block_on(async {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while view.model().unwrap().n_items() == 0 {
            assert!(std::time::Instant::now() < deadline, "live process table stayed empty");
            glib::timeout_future(std::time::Duration::from_millis(50)).await;
        }
        println!("Rendered {} process rows; window {}×{}", view.model().unwrap().n_items(),window.width(),window.height());
        assert!(window.width()<=440,"window too wide: {}",window.width());
        let browser=tree.iter().find(|w|w.widget_name()=="browser-cleanup").unwrap().clone();
        loop {
            let texts:Vec<_>=widgets(&browser).iter().filter_map(|w|w.clone().downcast::<gtk4::Label>().ok()).map(|l|l.text().to_string()).collect();
            if let Some(summary)=texts.iter().find(|t|t.contains("selectable") || t.contains("No browser connected")) {
                println!("Browser page: {summary}"); break;
            }
            assert!(std::time::Instant::now()<deadline,"browser page did not load");
            glib::timeout_future(std::time::Duration::from_millis(50)).await;
        }

        for label in tree.iter().filter_map(|w| w.clone().downcast::<gtk4::Label>().ok()) {
            if label.text().contains(" of ") || label.text().contains("PSI") {
                println!("{}", label.text());
            }
        }
    });
    window.close();
}
