//! A tokio runtime beside the GTK main loop, for Drive calls and sign-in.

use std::future::Future;
use std::sync::OnceLock;
use tokio::runtime::Runtime;
use tokio::task::AbortHandle;

pub fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

/// Runs `fut` on the runtime and hands its output to `done` on the GTK main thread.
pub fn run<T, F>(fut: F, done: impl FnOnce(T) + 'static) -> AbortHandle
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handle = runtime().spawn(fut);
    let abort = handle.abort_handle();
    gtk::glib::spawn_future_local(async move {
        if let Ok(v) = handle.await {
            done(v);
        }
    });
    abort
}

/// Runs blocking work (systemctl, file walks) off the main thread.
pub fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    run(
        async move { tokio::task::spawn_blocking(work).await.expect("blocking task") },
        done,
    );
}
