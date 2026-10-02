//! Network runtime kept alive while GTK owns the main thread.
pub fn network_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    // GTK owns the main thread. Workers must keep I/O and timers moving even
    // when no thread is inside Runtime::block_on.
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
}

#[cfg(all(test, feature = "gui"))]
mod tests {
    #[test]
    fn timers_progress_while_glib_owns_the_thread() {
        let runtime = super::network_runtime().unwrap();
        let _entered = runtime.enter();
        let context = gtk4::glib::MainContext::new();
        context.with_thread_default(|| {
            context.block_on(async {
                let timer = async {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    true
                };
                let watchdog = async {
                    gtk4::glib::timeout_future(std::time::Duration::from_millis(300)).await;
                    false
                };
                assert!(tokio::select! { ok = timer => ok, ok = watchdog => ok },
                    "Tokio's timer driver stalled while GLib owned the thread");
            });
        }).unwrap();
    }
}
