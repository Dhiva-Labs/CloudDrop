/// Desktop notifications, sent from a short-lived thread so callers never block on D-Bus.
#[derive(Clone)]
pub struct Notifier {
    enabled: bool,
    /// The connection they are about, added to every title: "Uploaded to Nextcloud (Work)".
    connection: Option<String>,
}

impl Notifier {
    pub fn new(enabled: bool) -> Self {
        Self { enabled, connection: None }
    }

    /// Titles name the connection (none for the default connection, whose read as they did).
    pub fn for_connection(enabled: bool, connection: Option<String>) -> Self {
        Self { enabled, connection }
    }

    /// The title as shown: `summary`, plus the connection's name.
    pub fn title(&self, summary: &str) -> String {
        match &self.connection {
            Some(name) => format!("{summary} ({name})"),
            None => summary.to_string(),
        }
    }

    pub fn send(&self, summary: &str, body: &str) {
        if !self.enabled {
            return;
        }
        let (summary, body) = (self.title(summary), body.to_string());
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
            .summary(&self.title(summary))
            .body(body)
            .icon("folder-remote")
            .show();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_name_the_connection() {
        assert_eq!(Notifier::new(true).title("Uploaded to Google Drive"), "Uploaded to Google Drive");
        let n = Notifier::for_connection(true, Some("Work".into()));
        assert_eq!(n.title("Uploaded to Nextcloud"), "Uploaded to Nextcloud (Work)");
    }
}
