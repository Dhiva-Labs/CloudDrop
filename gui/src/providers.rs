//! The clouds CloudDrop can upload to, as plain data for the setup assistant and the window.
//!
//! The app links the engine, so it reads [`Provider::ALL`] and [`Provider::info`] directly
//! (the Windows app runs `clouddrop providers --json` for the same list). What the engine
//! doesn't say (a one-line description, whether CloudDrop only sees its own folder, an icon)
//! comes from [`extras`]. Everything here is plain data, so tests and previews can build
//! providers the engine doesn't have yet.

use clouddrop::config::{self, Config};
use clouddrop::control;
use clouddrop::db::Kind;
use clouddrop::remote::{Field, FieldKind, Provider, SignIn};

/// One field of a sign-in form (see `remote::Field`).
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub required: bool,
    /// Example or explanation shown under the field.
    pub hint: String,
}

impl From<&Field> for FormField {
    fn from(f: &Field) -> Self {
        FormField { key: f.key.into(), label: f.label.into(), kind: f.kind, required: f.required, hint: f.hint.into() }
    }
}

/// How someone connects CloudDrop to a provider.
#[derive(Debug, Clone, PartialEq)]
pub enum SignInKind {
    /// Google's own flow, in-process (`signin::start`).
    Google,
    /// A browser sign-in through `clouddrop login --no-browser` (OneDrive, Dropbox).
    OAuth,
    /// A server address, then Nextcloud's Login Flow in the browser (`login --no-browser`).
    Nextcloud,
    /// Fields to fill in, checked with `clouddrop check`; no browser sign-in.
    Form(Vec<FormField>),
}

/// A provider as the setup assistant offers it and the window names it.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// `provider = "…"` in config.toml ("gdrive").
    pub id: String,
    /// "Google Drive", "OneDrive"…
    pub name: String,
    pub description: String,
    /// CloudDrop only sees the folder it creates (Drive's drive.file, app folders).
    pub own_folder: bool,
    /// No content hash: freeing up space downloads the cloud copy once to compare.
    pub verify_by_download: bool,
    /// Files and folders have a web page ("Open in Google Drive"). Guessed from a made-up id
    /// until the cloud folder exists; `Snapshot::load` then asks with its real id.
    pub web_links: bool,
    /// Whether the provider reports storage; None = it isn't known (read the quota).
    pub has_quota: Option<bool>,
    pub sign_in: SignInKind,
    /// Generic icon name (no brand logos).
    pub icon: &'static str,
}

const CLOUD_ICON: &str = "weather-overcast-symbolic";
const SERVER_ICON: &str = "network-server-symbolic";

/// What the engine doesn't say about a provider, by id: (description, own folder, icon).
pub fn extras(id: &str) -> (&'static str, bool, &'static str) {
    match id {
        "gdrive" => ("Google's cloud storage, with your Google account.", true, CLOUD_ICON),
        "onedrive" => ("Microsoft's cloud storage, with a personal or work account.", true, CLOUD_ICON),
        "dropbox" => ("Dropbox, with your Dropbox account.", true, CLOUD_ICON),
        "nextcloud" => ("Your own Nextcloud or ownCloud server.", false, SERVER_ICON),
        "webdav" => ("Any WebDAV server, such as a NAS.", false, SERVER_ICON),
        "s3" => (
            "Amazon S3 or a compatible bucket: Backblaze B2, Wasabi, Cloudflare R2, MinIO…",
            false,
            "drive-multidisk-symbolic",
        ),
        "sftp" => ("Any computer or server you can reach over SSH.", false, "utilities-terminal-symbolic"),
        "mega" => ("MEGA's end-to-end encrypted cloud storage.", false, "channel-secure-symbolic"),
        _ => ("", false, CLOUD_ICON),
    }
}

impl Choice {
    /// A provider from what the engine (or `providers --json`) says, with [`extras`] filled in.
    pub fn new(id: &str, name: &str, verify_by_download: bool, web_links: bool, sign_in: SignInKind) -> Self {
        let (description, own_folder, icon) = extras(id);
        let description = match (&sign_in, description) {
            (SignInKind::Form(_), "") => "Connect with a few details.",
            (_, d) => d,
        };
        Choice {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            own_folder,
            verify_by_download,
            web_links,
            has_quota: None,
            sign_in,
            icon,
        }
    }

    pub fn of(p: Provider) -> Self {
        let info = p.info();
        let sign_in = match &info.sign_in {
            SignIn::Google => SignInKind::Google,
            SignIn::OAuth(_) => SignInKind::OAuth,
            SignIn::NextcloudLoginFlow => SignInKind::Nextcloud,
            SignIn::Form(fields) => SignInKind::Form(fields.iter().map(FormField::from).collect()),
        };
        Choice::new(info.id, info.name, info.caps.hash.is_none(), p.web_url(Kind::Dir, "x").is_some(), sign_in)
    }

    /// The configured provider; Google Drive when there is no config.
    pub fn current(cfg: Option<&Config>) -> Self {
        Choice::of(cfg.map(|c| c.provider).unwrap_or_default())
    }

    pub fn is_google(&self) -> bool {
        self.id == "gdrive"
    }

    /// Fields to fill in, for form providers (no browser sign-in).
    pub fn form(&self) -> Option<&[FormField]> {
        match &self.sign_in {
            SignInKind::Form(fields) => Some(fields),
            _ => None,
        }
    }

    pub fn is_form(&self) -> bool {
        self.form().is_some()
    }

    /// Where the cloud folder lives: "My Drive" for Google Drive, else the provider's name.
    pub fn root_label(&self) -> &str {
        if self.is_google() { "My Drive" } else { &self.name }
    }

    /// "Open in {P}" is offered.
    pub fn can_open_web(&self) -> bool {
        self.web_links
    }

    /// Whether this provider's sign-in is saved on this computer.
    ///
    /// Google Drive keeps `token.json` (`control::is_signed_in`). The others follow
    /// docs/PROVIDERS.md: OAuth tokens in `token-<id>.json`, other secrets (Nextcloud's app
    /// password, form passwords and keys) under the id in `credentials.json`. A form without
    /// required secrets (SFTP through the SSH agent) needs nothing saved.
    pub fn signed_in(&self) -> bool {
        let saved = || clouddrop::credentials::load(&self.id).ok().flatten().is_some();
        match &self.sign_in {
            SignInKind::Google => control::is_signed_in(),
            SignInKind::OAuth => config::data_dir().join(format!("token-{}.json", self.id)).exists(),
            SignInKind::Nextcloud => saved(),
            SignInKind::Form(fields) => saved() || !fields.iter().any(|f| f.kind == FieldKind::Secret && f.required),
        }
    }
}

/// A config the app can use: Google Drive needs a client; other providers need nothing more.
pub fn usable(cfg: &Config) -> bool {
    !cfg.provider.is_default() || !cfg.client_id.is_empty()
}

/// Whether CloudDrop is set up (`control::is_configured` for Google Drive, which also wants
/// the client secret).
pub fn is_configured() -> bool {
    Config::load().is_ok_and(|c| if c.provider.is_default() { control::is_configured() } else { usable(&c) })
}

/// Every provider the setup assistant offers, the engine's first.
///
/// `CLOUDDROP_FAKE_PROVIDERS=1` adds [`fakes`] for previews and development only, so the
/// OAuth, Nextcloud and form pages can be seen before those providers exist. Their setup
/// fails at the CLI (it doesn't know them) and shows that error on the page.
pub fn all() -> Vec<Choice> {
    let mut list: Vec<Choice> = Provider::ALL.iter().map(|p| Choice::of(*p)).collect();
    if std::env::var("CLOUDDROP_FAKE_PROVIDERS").is_ok_and(|v| v == "1") {
        for f in fakes() {
            if !list.iter().any(|c| c.id == f.id) {
                list.push(f);
            }
        }
    }
    list
}

/// Stand-ins for providers being built (one of each sign-in kind, and every field kind).
pub fn fakes() -> Vec<Choice> {
    let field = |key: &str, label: &str, kind, required, hint: &str| FormField {
        key: key.into(),
        label: label.into(),
        kind,
        required,
        hint: hint.into(),
    };
    let mut s3 = Choice::new(
        "s3",
        "S3",
        false,
        false,
        SignInKind::Form(vec![
            field("endpoint", "Endpoint", FieldKind::Url, true, "https://s3.us-east-1.amazonaws.com"),
            field("region", "Region", FieldKind::Text, false, "For example us-east-1"),
            field("bucket", "Bucket", FieldKind::Text, true, "The bucket must exist already"),
            field("prefix", "Folder in the bucket", FieldKind::Text, false, "Leave empty for the whole bucket"),
            field("access_key_id", "Access key ID", FieldKind::Text, true, "A key that can only use this bucket"),
            field("secret_access_key", "Secret access key", FieldKind::Secret, true, ""),
        ]),
    );
    s3.has_quota = Some(false);
    vec![
        Choice::new("onedrive", "OneDrive", false, true, SignInKind::OAuth),
        Choice::new("nextcloud", "Nextcloud", false, true, SignInKind::Nextcloud),
        Choice::new(
            "webdav",
            "WebDAV",
            true,
            false,
            SignInKind::Form(vec![
                field("url", "Server address", FieldKind::Url, true, "https://cloud.example.com/remote.php/dav/files/me"),
                field("user", "User name", FieldKind::Text, true, ""),
                field("password", "Password", FieldKind::Secret, true, "An app password, if your server offers them"),
            ]),
        ),
        s3,
        Choice::new(
            "sftp",
            "SFTP",
            true,
            false,
            SignInKind::Form(vec![
                field("host", "Server", FieldKind::Text, true, "A name or address, such as nas.local"),
                field("port", "Port", FieldKind::Number, false, "22 if empty"),
                field("user", "User name", FieldKind::Text, true, ""),
                field("path", "Folder on the server", FieldKind::Text, true, "For example /home/me/backup"),
                field("key_file", "SSH key", FieldKind::File, false, "Empty: use the SSH agent"),
                field("passphrase", "Key passphrase", FieldKind::Secret, false, ""),
            ]),
        ),
    ]
}

// ---------- forms ----------

/// Whether a form value can be used; Err(why) to show under the field.
/// Empty values are fine here; [`form_ready`] wants the required ones filled.
pub fn check_value(kind: FieldKind, value: &str) -> Result<(), &'static str> {
    let v = value.trim();
    if v.is_empty() {
        return Ok(());
    }
    match kind {
        FieldKind::Url if !looks_like_web_address(v) => Err("Enter an address that starts with https://"),
        FieldKind::Number if !v.chars().all(|c| c.is_ascii_digit()) => Err("Enter a number"),
        _ => Ok(()),
    }
}

/// "https://…" or "http://…" with something after it.
pub fn looks_like_web_address(v: &str) -> bool {
    let v = v.trim();
    ["https://", "http://"].iter().any(|p| v.len() > p.len() && v.starts_with(p))
}

fn filled(f: &FormField, value: &str) -> bool {
    if f.kind == FieldKind::Secret { !value.is_empty() } else { !value.trim().is_empty() }
}

/// Every required field is filled and every value is valid (`values` in field order).
pub fn form_ready(fields: &[FormField], values: &[String]) -> bool {
    fields.len() == values.len()
        && fields
            .iter()
            .zip(values)
            .all(|(f, v)| (!f.required || filled(f, v)) && check_value(f.kind, v).is_ok())
}

/// A setting or secret for `clouddrop setup` (`key=value`).
pub type KeyValue = (String, String);

/// Splits filled-in values into settings (`--set`) and secrets (`--secret`). Empty optional
/// fields are left out; secrets are passed as typed, the rest trimmed.
pub fn form_values(fields: &[FormField], values: &[String]) -> (Vec<KeyValue>, Vec<KeyValue>) {
    let (mut settings, mut secrets) = (Vec::new(), Vec::new());
    for (f, v) in fields.iter().zip(values) {
        if !filled(f, v) {
            continue;
        }
        if f.kind == FieldKind::Secret {
            secrets.push((f.key.clone(), v.clone()));
        } else {
            settings.push((f.key.clone(), v.trim().to_string()));
        }
    }
    (settings, secrets)
}

/// The field's label, with " (optional)" when it may stay empty.
pub fn field_title(f: &FormField) -> String {
    if f.required { f.label.clone() } else { format!("{} (optional)", f.label) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_id(id: &str) -> Choice {
        fakes().into_iter().find(|c| c.id == id).unwrap()
    }

    #[test]
    fn google_drive_from_the_engine() {
        let g = Choice::of(Provider::GoogleDrive);
        assert_eq!((g.id.as_str(), g.name.as_str()), ("gdrive", "Google Drive"));
        assert_eq!(g.description, "Google's cloud storage, with your Google account.");
        assert!(g.own_folder && g.web_links && g.can_open_web() && g.is_google());
        assert!(!g.verify_by_download, "Drive reports an MD5");
        assert_eq!(g.sign_in, SignInKind::Google);
        assert_eq!(g.icon, "weather-overcast-symbolic");
        assert_eq!(g.root_label(), "My Drive");
        assert_eq!(Choice::current(None), g);
        assert_eq!(Choice::current(Some(&Config::default())), g);
        assert_eq!(all()[0], g, "the engine's providers come first, Google Drive by default");
    }

    #[test]
    fn extras_by_id() {
        assert_eq!(extras("nextcloud"), ("Your own Nextcloud or ownCloud server.", false, "network-server-symbolic"));
        assert_eq!(extras("s3").2, "drive-multidisk-symbolic");
        assert_eq!(extras("sftp").2, "utilities-terminal-symbolic");
        assert_eq!(extras("mega"), ("MEGA's end-to-end encrypted cloud storage.", false, "channel-secure-symbolic"));
        assert!(extras("onedrive").1 && extras("dropbox").1);
        assert_eq!(extras("floppy"), ("", false, "weather-overcast-symbolic"));
        let other = Choice::new("floppy", "Floppy", true, false, SignInKind::Form(vec![]));
        assert_eq!(other.description, "Connect with a few details.");
        let oauth = Choice::new("floppy", "Floppy", false, true, SignInKind::OAuth);
        assert_eq!(oauth.description, "");
    }

    #[test]
    fn fake_providers_cover_every_kind() {
        let one = by_id("onedrive");
        assert_eq!((one.sign_in.clone(), one.own_folder, one.root_label()), (SignInKind::OAuth, true, "OneDrive"));
        assert!(!one.is_google() && !one.is_form());
        assert_eq!(by_id("nextcloud").sign_in, SignInKind::Nextcloud);
        let dav = by_id("webdav");
        assert!(dav.verify_by_download && !dav.can_open_web() && dav.is_form());
        assert_eq!(dav.icon, "network-server-symbolic");
        assert_eq!(by_id("s3").has_quota, Some(false));
        let kinds: Vec<FieldKind> = fakes().iter().filter_map(|c| c.form()).flatten().map(|f| f.kind).collect();
        for k in [FieldKind::Text, FieldKind::Url, FieldKind::Number, FieldKind::Secret, FieldKind::File] {
            assert!(kinds.contains(&k), "{k:?}");
        }
    }

    #[test]
    fn configs_the_app_can_use() {
        assert!(!usable(&Config::default()), "Google Drive without a client isn't set up");
        let c = Config { client_id: "x.apps.googleusercontent.com".into(), ..Config::default() };
        assert!(usable(&c));
    }

    #[test]
    fn form_values_are_checked_by_kind() {
        assert_eq!(check_value(FieldKind::Url, "cloud.example.com"), Err("Enter an address that starts with https://"));
        assert_eq!(check_value(FieldKind::Url, "https://"), Err("Enter an address that starts with https://"));
        assert_eq!(check_value(FieldKind::Url, " https://cloud.example.com "), Ok(()));
        assert_eq!(check_value(FieldKind::Url, "http://nas.local:8080"), Ok(()));
        assert_eq!(check_value(FieldKind::Number, "22"), Ok(()));
        assert_eq!(check_value(FieldKind::Number, "2x"), Err("Enter a number"));
        assert_eq!(check_value(FieldKind::Number, "-1"), Err("Enter a number"));
        assert_eq!(check_value(FieldKind::Text, "anything"), Ok(()));
        assert_eq!(check_value(FieldKind::Url, ""), Ok(()), "emptiness is form_ready's business");
    }

    #[test]
    fn a_form_is_ready_when_required_fields_are_filled_and_valid() {
        let dav = by_id("webdav");
        let fields = dav.form().unwrap();
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(!form_ready(fields, &v(&["", "", ""])));
        assert!(!form_ready(fields, &v(&["https://dav.example.com", "me", ""])), "password missing");
        assert!(!form_ready(fields, &v(&["dav.example.com", "me", "pw"])), "not a web address");
        assert!(!form_ready(fields, &v(&["https://dav.example.com", "  ", "pw"])), "blank user");
        assert!(form_ready(fields, &v(&["https://dav.example.com", "me", "pw"])));
        assert!(!form_ready(fields, &v(&["https://dav.example.com", "me"])), "one value per field");

        let sftp = by_id("sftp");
        let fields = sftp.form().unwrap();
        assert!(form_ready(fields, &v(&["nas", "", "me", "/backup", "", ""])), "optional fields may stay empty");
        assert!(!form_ready(fields, &v(&["nas", "22a", "me", "/backup", "", ""])));
        assert_eq!(field_title(&fields[1]), "Port (optional)");
        assert_eq!(field_title(&fields[0]), "Server");
    }

    #[test]
    fn secrets_are_kept_apart_from_settings() {
        let sftp = by_id("sftp");
        let fields = sftp.form().unwrap();
        let values: Vec<String> =
            ["nas.local ", "", "me", "/backup", "/home/me/.ssh/id_ed25519", " pass phrase "].map(String::from).to_vec();
        let (settings, secrets) = form_values(fields, &values);
        let kv = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(
            settings,
            [kv("host", "nas.local"), kv("user", "me"), kv("path", "/backup"), kv("key_file", "/home/me/.ssh/id_ed25519")]
        );
        assert_eq!(secrets, [kv("passphrase", " pass phrase ")], "secrets are passed as typed");
    }
}
