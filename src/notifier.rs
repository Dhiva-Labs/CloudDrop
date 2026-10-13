/// Desktop notifications, sent from a short-lived thread so callers never block on D-Bus.
#[derive(Clone)]
pub struct Notifier {
    enabled: bool,
}

impl Notifier {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    pub fn send(&self, summary: &str, body: &str) {
        if !self.enabled {
            return;
        }
        let (summary, body) = (summary.to_string(), body.to_string());
        std::thread::spawn(move || {
            if let Err(e) = notify_rust::Notification::new()
                .appname("CloudDrop")
                .summary(&summary)
                .body(&body)
                .icon("folder-remote")
                .show()
            {
                tracing::debug!("notification failed: {e}");
            }
        });
    }

    /// Like `send`, but waits so a short-lived CLI process doesn't exit first.
    pub fn send_blocking(&self, summary: &str, body: &str) {
        if !self.enabled {
            return;
        }
        let _ = notify_rust::Notification::new()
            .appname("CloudDrop")
            .summary(summary)
            .body(body)
            .icon("folder-remote")
            .show();
    }
}
