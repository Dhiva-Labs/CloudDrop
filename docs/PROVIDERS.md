# Plan: more clouds than Google Drive

Status: **phase 0 is done** (the provider seam, below); everything after it is still a plan.
Target: OneDrive, Dropbox, MEGA, Proton Drive, and self-hosted storage (Nextcloud/ownCloud and
other WebDAV servers, S3-compatible buckets, SFTP).

## What must not change

Every provider has to keep CloudDrop's three promises, or it doesn't ship:

1. **Upload-only.** Local adds and edits go up; a local delete never deletes anything in the cloud.
2. **Free up space only after proof.** A local copy is removed only after the cloud is shown to
   hold identical content (today: Drive's `md5Checksum` and size, file not trashed).
3. **Old content is never lost.** When a cloud-only file is replaced by different content, the
   previous version is kept forever (today: Drive `keepForever` on the old revision).

Least privilege wherever the provider allows it: today CloudDrop only sees the files it created
(`drive.file`). The equivalent elsewhere is an **app folder** (`Apps/CloudDrop`).

## Providers at a glance

| | Sign-in | Least privilege | Resumable upload | Proof before freeing | Keep old version | Quota | Effort |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **Google Drive** (today) | OAuth, browser | `drive.file` | resumable session | `md5Checksum` | `keepForever` | `about.get` | — |
| **OneDrive** (personal + work) | OAuth + PKCE, browser, no client secret | `Files.ReadWrite.AppFolder` → `Apps/CloudDrop` | upload session, `nextExpectedRanges` | `quickXorHash` | versions are automatic, cannot be pinned → keep-aside copy | `/me/drive` quota | M |
| **Dropbox** | OAuth + PKCE, browser, offline refresh token | "App folder" app → `Apps/CloudDrop` | upload session (start / append / finish), resume by offset | `content_hash` (SHA-256 of 4 MiB block hashes) | history is 30–180 days, cannot be pinned → keep-aside copy | `get_space_usage` | M |
| **Nextcloud / ownCloud** | Login Flow v2 (browser hands back an app password) | the user's own account, one folder | chunked upload v2 | verify by download (`oc:checksums` SHA1 as an extra check) | versions app; keep-aside copy | `quota-*-bytes` | M |
| **Other WebDAV** | URL, user, app password | one folder | none: a failed upload starts over | none → verify by download | keep-aside copy (`COPY`) | `quota-*-bytes` if offered | S (after Nextcloud) |
| **S3-compatible** (AWS, Backblaze B2, Wasabi, R2, MinIO, Garage) | endpoint, bucket, access key | a key scoped to one bucket + prefix | multipart, resume via `ListParts` | `x-amz-checksum-sha256` where supported, else single-part ETag (MD5), else verify by download | bucket versioning, else keep-aside copy | none ("CloudDrop holds …") | M |
| **SFTP** | host-key check (shown, trust on first use), SSH key or agent | one directory | write at offset | none → `sha256sum` over SSH if allowed, else verify by download | keep-aside copy | `statvfs@openssh.com` | M |
| **MEGA** | email + password (+2FA) → session | whole account (no app folder), one folder used | via SDK | none server-side (end-to-end encrypted) → verify by download | MEGA versioning; keep-aside copy | account quota | L |
| **Proton Drive** | Proton login (SRP, 2FA, maybe a second password) → session | whole account, one folder used | via SDK (encrypted blocks) | via SDK (signed manifests; plaintext digest in encrypted attributes) or verify by download | revisions; keep-aside copy | account quota | XL, blocked (below) |

S/M/L/XL are relative sizes once the provider seam (phase 0) exists.

**Keep-aside copy**: where a version can't be pinned, before replacing a cloud-only file the
engine first makes a server-side copy named `name (replaced 2026-10-21).ext` next to it, then
uploads the new content. It's the same idea as today's `name (2).ext` rule for overwrites.

**Verify by download**: for providers without a server-side hash, freeing downloads the file once
and compares a local SHA-256 before deleting the local copy. It's slower and uses bandwidth, so
Home and Files say "Verifying…". It is the default for MEGA, generic WebDAV and SFTP. A setting can
change it to "trust a completed upload" (size check only), with a clear warning.

### Provider notes

- **OneDrive.** `quickXorHash` is the hash for both personal and work accounts (SHA-1 was
  deprecated on personal accounts in 2023), and it's simple to compute while uploading. Check
  for the property on every item instead of assuming it. Fragments must be multiples of
  320 KiB. Work accounts may need admin consent; multi-tenant apps should go through publisher
  verification. Register a public client (no secret), so source builds work without a key of
  your own, unlike Google.
- **Dropbox.** A new app starts in development mode, limited to a small number of users, and
  needs Dropbox's production approval before release. Names are case-insensitive. Honour 429
  with `Retry-After`.
- **S3.** There are no folders: a rename is a server-side copy plus a delete of the old key.
  The engine deletes the old key only after the copy's checksum matches, and only when the
  bucket has versioning (otherwise it keeps the old key). Document this exception to "never
  deletes". `x-amz-checksum-*` support varies between S3-compatible services; probe it at
  setup.
- **SFTP.** Upload to `.clouddrop-<id>.part` and rename into place, so a half-written file never
  looks complete. Pin the host key in the config.
- **MEGA.** The official C++ SDK is BSD-2 licensed and does the crypto, transfers and resume,
  but it is a large C++ build (SWIG bindings, its own dependencies) that has to be vendored for
  the offline PPA build and cross-built for Windows. The other option is a native Rust client of
  MEGA's protocol (community crates exist, maturity varies). **Decide with a one-week spike**:
  resume after a crash, 10 GB files, 2FA, session reuse.
- **Proton Drive.** Proton's SDK (JavaScript and C#, with Swift and Kotlin bindings) is **not
  yet supported for third-party integrations**. Authentication isn't in it yet, and general
  availability with a new cryptographic model is targeted for end 2026 / early 2027. Proton has
  said third-party clients will eventually have to use the SDK or match its safety features.
  Community Rust ports exist but are unofficial and early. **Plan: build everything else first,
  and ship Proton as "experimental" once the official SDK supports third-party sign-in**,
  through a small sidecar process if no C ABI is offered. Follow Proton's naming and branding
  rules for third-party apps. Never store the Proton password, only the session.

### WebDAV / Nextcloud

One client (`src/webdav.rs`) for two providers, because Login Flow v2 exists only on Nextcloud
and capabilities are static:

| | `webdav` ("WebDAV") | `nextcloud` ("Nextcloud") |
| --- | --- | --- |
| Sign-in | `Form`: `url`, `user`, `password` (secret), `folder` | `NextcloudLoginFlow` |
| Config / credentials | `[webdav]`, `credentials.json` › `webdav.password` | `[nextcloud]`, `credentials.json` › `nextcloud.password` |
| `resumable` | false | true (chunks stay on the server) |

Both: `hash: None`, `pin_versions: false`, `stable_ids: false`, `forbidden_chars: "\"`,
`chunk_unit` 1 MiB, no web links. ownCloud and Nextcloud servers set up as `webdav` still get
chunked uploads and checksums: the client looks at the server, whatever the provider.

- **The server.** `status.php` says whether it's Nextcloud/ownCloud. If it is, files are at
  `remote.php/dav/files/<user id>` (the id comes from the DAV principal and may differ from the
  login name), and chunked uploads are on when OCS reports `dav.chunking`. Anything else is
  plain WebDAV at `url`. `WebDav::probe()` does this and checks the password;
  `clouddrop setup` and `check` should save its `ServerInfo` as the section's `server`, so later
  starts skip it (without it, the client looks once per start).
- **Ids** are paths below the files' top level: `/`, `/Backups/CloudDrop/a.txt`. A child's id
  is its parent's id + `/` + name, so after a folder move the engine swaps the prefix
  `old + "/"` for `new + "/"` beneath it. `top_parent` is `/<folder>`, made if missing.
- **Uploads.** The whole file in one chunk: one PUT. More chunks on Nextcloud: chunked upload
  v2 (MKCOL `uploads/<id>/<session>`, PUT `00001`…, MOVE `.file` with `Destination`,
  `OC-Total-Length` and `OC-Checksum: SHA1:`). `query_upload` lists the chunks the server
  holds, so another process continues there. Chunks under 5 MiB (Nextcloud on S3 needs that)
  and other servers get one PUT with a known length whose body is fed chunk by chunk;
  `query_upload` answers `Next(0)` for those.
- **Proof.** Freeing verifies by download. `oc:checksums` is whatever the uploading client
  said, so it's only an extra check: after an upload (with Nextcloud's own `X-Hash-SHA1` of the
  bytes it received) and on downloads (`OC-Checksum` must match the content).
- **Never overwriting.** `keep_version` COPYs to `<stem> (replaced YYYY-MM-DD).<ext>`. MOVE and
  COPY send `Overwrite: F`; a file CloudDrop doesn't know that sits where an upload or a rename
  has to go is first moved aside under that same name. A folder in the way is an error.
- **Errors.** 401 `Auth`; 404 and 409 (missing parent) `NotFound`; 429, and 503 with
  `Retry-After`, `RateLimited`; 507 `Quota`; 408, 423 and other 5xx `Transient`.
- **`clouddrop login`** for `SignIn::NextcloudLoginFlow`: `webdav::nextcloud_login_start(url)`,
  show `flow.login_url` (open the browser), then `webdav::nextcloud_login_poll(&flow)`, which
  waits (every 2 s, up to 20 minutes) and returns the server, login name and app password.
  `login.settings(&cfg.nextcloud)` is the new `[nextcloud]` section and
  `credentials::save("nextcloud", login.secrets())` stores the password.
- **Tests:** `tests/webdav.rs` against `tests/common/webdav_mock.rs`, in plain and Nextcloud
  mode.

## Engine changes (phase 0, no visible change)

**Done**, with these differences from the list below, each deferred to the first provider that
needs it:

- `RemoteError` keeps today's four kinds (`Auth`, `Transient`, `NotFound`, `Permanent`);
  `RateLimited` and `Quota` arrive with OneDrive/Dropbox, whose errors need them.
- `ContentHash` is in place at the seam (provider metadata, downloads, the free check); local
  hashing is still the MD5 helpers, since MD5 is the only kind so far.
- Config keeps writing `drive_folder` (the Windows app and `config set` use that key) and reads
  `remote_folder` too. `provider` is only written when it isn't Google Drive.
- `provider` is in `status --json` and in `meta`, where the engine refuses a database bound to
  another provider. `status.json` is unchanged (the apps read the provider from the config).
  `meta.account` waits for a provider-neutral way to name the account.


The Google coupling is narrow: `Drive` in `src/drive.rs` has about a dozen calls, used from
`engine.rs`, `jobs.rs`, `actions.rs` and the Windows agent (`win/src/cloud.rs`: metadata check
before freeing, ranged download when a placeholder opens). Plus `auth.rs`, and 97 "Google
Drive" strings across the apps.

1. **`Remote` seam.** An enum with one variant per provider (`Remote::GoogleDrive(..)`,
   `Remote::OneDrive(..)`, …) rather than `dyn` plus `async_trait`: no new dependency, and every
   call site sees every provider. Operations: `capabilities()`, `ensure_root(name)`,
   `find_child`, `create_folder`, `get(id) -> RemoteMeta`, `move_rename`,
   `keep_version(id)` (pin, or keep-aside copy), `start_upload` / `query_upload` / `put_chunk`
   (the session is an opaque string kept in `upload_url` as today), `download_range`,
   `download`, `quota()`, `web_url(id)`.
   `capabilities()` reports the hash kind, whether resumable, pin vs copy, real folders vs
   prefixes, name rules (forbidden characters, case-insensitive, maximum length).
2. **Errors.** `DriveError` becomes `RemoteError { Auth, NotFound, Quota, RateLimited(retry_after),
   Transient, Permanent }`, so backoff and status phases (`needs_sign_in`, `offline`,
   `drive_folder_missing` → `remote_folder_missing`) stay provider-neutral.
3. **Content hash.** `ContentHash { kind, value }` with local hashers for MD5, quickXor,
   Dropbox content hash, SHA-1 and SHA-256, computed while the file streams up and again when
   freeing.
4. **Database v3.** Keep the column names (`drive_id`, `md5`) so older Nautilus extensions and
   Windows apps keep reading the database. Add `hash_kind` (NULL = md5) and meta keys
   `provider` and `account`. Document that `drive_id` is "the provider's id".
5. **Config.** `provider = "gdrive"` (default when absent), plus one section per provider:
   `[onedrive]`, `[dropbox]`, `[webdav] url user`, `[s3] endpoint region bucket prefix`,
   `[sftp] host port user path host_key`. `drive_folder` becomes `remote_folder`, with the old
   key still read.
6. **Secrets.** Today `token.json` (0600). Move to the OS keyring (Secret Service on Linux,
   Credential Manager on Windows) with the file as fallback. The snap needs the
   `password-manager-service` interface, which is a manual store review, so the snap keeps the
   file until that's granted.
7. **Status.** `status.json` gains `provider` and `account` (display name / email); `quota` keeps
   its shape, and `limit: null` already means "unlimited / unknown".

## Adding a provider (the contract)

Everything a provider needs from the rest of CloudDrop is fixed here, so providers, the engine
and the apps can be built at the same time.

### Files

A provider called `<id>` (`onedrive`, `dropbox`, `webdav`, `s3`, `sftp`, `mega`):

- **`src/<id>.rs`**: the client struct and `pub static INFO: ProviderInfo`, plus its unit tests.
- **`tests/common/<id>_mock.rs`**: a fake server in axum, like the Google Drive one in
  `tests/common/mod.rs`. It must cover resumable sessions, hashes, errors (401 then refresh,
  429/503, storage full) and missing parents.
- **The few shared lines** it may add, and nothing else in shared files:
  - `src/lib.rs`: `pub mod <id>;` and one arm in `remote_for`.
  - `src/remote.rs`: a `Provider` variant (with `#[serde(rename = "<id>")]`), its entry in
    `Provider::ALL` and `Provider::info`, a `Remote` variant, its arm in `Remote::provider` and
    one arm in `each_client!`.
  - `src/config.rs`: a `pub <id>: <Id>Config` section struct with `#[serde(default)]` and
    `skip_serializing_if` so existing files don't change.
  - `tests/common/mod.rs`: `pub mod <id>_mock;`.
  - `Cargo.toml`: its dependencies. Prefer what's already there: `reqwest`, `serde`, `tokio`,
    `sha2`/`sha1`/`md-5`, `base64`, `hex`. Every new crate is vendored into the offline PPA build.

### The client

Inherent async methods with exactly these signatures (`Remote` calls them through
`each_client!`, so the compiler checks the shape):

```rust
pub async fn storage_quota(&self) -> RemoteResult<StorageQuota>
pub async fn top_parent(&self) -> RemoteResult<String>
pub async fn get(&self, id: &str) -> RemoteResult<RemoteMeta>
pub async fn find_child(&self, name: &str, parent: &str, folder: bool) -> RemoteResult<Vec<RemoteMeta>>
pub async fn create_folder(&self, name: &str, parent: &str) -> RemoteResult<RemoteMeta>
pub async fn move_rename(&self, id: &str, name: Option<&str>, parents: Option<(&str, &str)>) -> RemoteResult<RemoteMeta>
pub async fn keep_version(&self, id: &str) -> RemoteResult<()>
pub async fn start_upload(&self, existing: Option<&str>, name: &str, parents: (&str, Option<&str>), size: u64, mime: &str) -> RemoteResult<String>
pub async fn query_upload(&self, session: &str, total: u64) -> RemoteResult<UploadStatus>
pub async fn put_chunk(&self, session: &str, data: bytes::Bytes, start: u64, total: u64) -> RemoteResult<UploadStatus>
pub async fn download_range(&self, id: &str, start: u64, end: u64) -> RemoteResult<bytes::Bytes>
pub async fn download(&self, id: &str, out: &mut tokio::fs::File) -> RemoteResult<(ContentHash, u64)>
```

Rules, as `src/drive.rs` follows them:

- **Ids are opaque strings.** Where the provider has no stable ids, use paths relative to the
  provider's root and set `stable_ids: false`. `move_rename` then returns the new id; the engine
  rewrites the ids of everything beneath a moved folder.
- **`top_parent`** is where the top `CloudDrop` folder goes: My Drive (`"root"`), the app folder
  (OneDrive `approot`, Dropbox's app folder), or the configured WebDAV path, S3 prefix or SFTP
  directory.
- **`find_child` never returns trashed items.** `get` reports `trashed: true` rather than
  NotFound when the provider has a trash.
- **`start_upload`** returns an opaque session string that the engine persists, so after a
  crash `query_upload` gets the same string. Encode what you need, e.g. JSON with the upload URL
  and the target path. `put_chunk` gets chunks that are a multiple of `caps.chunk_unit`, except
  the last; return `Next(offset)` until the provider confirms the whole file, then
  `Done(meta)`. A session the provider no longer knows is `NotFound`. Without resumable
  uploads (`resumable: false`), `query_upload` always answers `Next(0)`.
- **`keep_version`** pins the current version (`pin_versions: true`). Otherwise it makes a
  server-side copy named `<stem> (replaced YYYY-MM-DD).<ext>` in the same folder, so replacing
  the content never loses it.
- **`RemoteMeta.hash`** is the provider's hash of kind `caps.hash`, as lowercase hex. Convert
  base64 (OneDrive). Leave it `None` when the provider reports none, and set `caps.hash: None`.
  `download` returns the hash of what it wrote, in the same kind, or SHA-256 when `caps.hash` is
  None. Use `crate::hash::Hasher`.
- **Errors:**
  - `Auth`: sign-in needed.
  - `Transient`: network trouble or 5xx.
  - `RateLimited`: 429, with `Retry-After` when given.
  - `Quota`: storage full.
  - `NotFound`.
  - `Permanent`: anything else. The message shows to people, so make it a short sentence.
- **Never delete anything in the cloud,** except S3's rename: copy, check the checksum, then
  delete the old key, and only when the bucket has versioning.

### Sign-in and secrets

- **`INFO.sign_in`** describes how people connect: `OAuth(OAuthApp)`, `NextcloudLoginFlow` or
  `Form(fields)`.
- **OAuth clients** take an `Arc<dyn TokenSource>` (`remote::TokenSource`). The generic browser
  sign-in implements it over `token-<id>.json` in the data directory; tests use a fixed token.
- **Other secrets** come from `crate::credentials::load("<id>")`. Non-secret settings come from
  the `[<id>]` config section.

### The engine (built alongside the providers)

The engine reads `Capabilities` and does the rest; providers don't touch `engine.rs`,
`jobs.rs` or `actions.rs`:

- **Hashing:** it hashes local files in `caps.hash`'s kind and records it in `entries.hash_kind`.
- **Freeing:** it proves the cloud copy by downloading it when `caps.hash` is None.
- **Moves:** it rewrites descendant ids when `stable_ids` is false.
- **Names:** it maps `forbidden_chars`, and avoids case-only clashes where `case_insensitive`.
- **Errors:** it treats `RateLimited` like `Transient`, honouring `retry_after`, and `Quota`
  like `Permanent`.

### What the apps read (the CLI contract)

`clouddrop providers --json` lists every provider:

```json
[{"id": "webdav", "name": "WebDAV", "verify_by_download": true, "web_links": false,
  "sign_in": {"kind": "form", "fields": [{"key": "url", "label": "Server address", "kind": "url",
                                          "required": true, "hint": "https://cloud.example.com/remote.php/dav/files/me"}]}}]
```

`sign_in.kind` is `google`, `oauth`, `nextcloud` or `form`.

| Command | What it does |
| --- | --- |
| `clouddrop setup --provider <id> [--set key=value]… [--secret key=value]…` | Writes the provider's config section and credentials. |
| `clouddrop login [--no-browser]` | Runs the provider's browser sign-in, if it has one. |
| `clouddrop check` | Connects with the saved settings and says whether it works. |

`status --json` gains `"provider_name"` (e.g. `"OneDrive"`) next to `"provider"`.

## App changes

- **Setup assistant**: a new first step, *Choose where to upload*, with a card per provider
  (logo, one-line description, "CloudDrop only sees its own folder" where true). Then the
  provider's sign-in: browser OAuth (Drive, OneDrive, Dropbox), Nextcloud Login Flow, or a short
  form (WebDAV, S3, SFTP, MEGA) with a **Test Connection** button before continuing.
- **Provider name and icon** replace "Google Drive" in Home's storage card, the Files summary,
  context menus ("Open in OneDrive"; hidden when there's no web link), notifications, the tray
  menu, Nautilus and Explorer. One `provider.display_name()` in the engine; the apps read it from
  `status --json`.
- **Settings › Account**: provider logo, account, cloud folder, *Sign Out*, *Switch Provider…*.
  Switching means a fresh upload into the new provider, the same flow as `reset-cloud`; nothing
  is deleted from the old one.
- **Home**: quota when the provider has one, otherwise "CloudDrop holds 25.3 GB". "Verifying…"
  joins "Uploading" for providers that verify by download.
- Provider logos follow each brand's guidelines (Google, Microsoft, Dropbox, MEGA and Proton all
  publish them) and appear only where they name the destination.

## Phases

| Phase | What | Done when |
| --- | --- | --- |
| 0 | `Remote` seam, `ContentHash`, DB v3, provider-neutral strings, `provider` in status. Google Drive only. | All current tests pass unchanged; Drive behaviour identical. |
| 1 | **OneDrive** and **Dropbox**. Mock servers for both in `tests/common`, like the Drive one. Setup provider picker. | e2e suite green against all three mocks; live smoke test with real accounts. |
| 2 | **Self-hosted**: Nextcloud (first-class), generic WebDAV, S3-compatible, SFTP. Verify-by-download and keep-aside copy land here. | e2e against real servers in CI containers: Nextcloud, MinIO, OpenSSH. |
| 3 | **MEGA** (SDK or native, from the spike). | Resume, 2FA and verify-by-download tested; PPA and Windows builds reproducible. |
| 4 | **Proton Drive**, experimental, when third-party sign-in exists in the official SDK. | Proton's third-party terms met; their review if they require one. |
| 5 | **Several clouds at once**: one engine per connection (own folder, config section, `state-<id>.db`, `status-<id>.json`); the apps get a connection switcher. | Two providers syncing side by side. |

## Testing

- **One conformance suite for every `Remote`**: create, upload and resume after a cut, rename and
  move, keep version, ranged download, hash agreement, 401 then refresh, 429/503 backoff, quota
  full, folder deleted remotely. It runs against each mock, and against real backends when
  `CLOUDDROP_LIVE_<PROVIDER>` credentials are set.
- **The existing e2e tests (`tests/e2e.rs`) run once per provider.**
- **Self-hosted backends run for real in CI** (Nextcloud, MinIO and OpenSSH containers), which is
  cheap and catches more than a mock.

## Open questions

- Phase 1 order: OneDrive first (more users; File Explorer shows it natively, which may confuse,
  so test that) or Dropbox first (simplest API)?
- Is "verify by download" acceptable as the default for MEGA, WebDAV and SFTP, or is a size-only
  "trust upload" preferable for very large files?
- Phase 5 (several clouds at once): is it wanted at all, or is one provider per installation
  enough?

## Sources

- Proton: [Drive SDK update, January 2026](https://proton.me/blog/drive-sdk-january-2026),
  [SDK repository](https://github.com/ProtonDriveApps/sdk/),
  [rclone forum thread with Proton's statement](https://forum.rclone.org/t/proton-drive-x-rclone/53609)
- Microsoft Graph: [createUploadSession](https://learn.microsoft.com/graph/api/driveitem-createuploadsession),
  [sha1Hash deprecation on OneDrive Personal](https://devblogs.microsoft.com/microsoft365dev/deprecation-of-sha1hash-on-onedrive-personal/)
- MEGA: [megasdk packaging (BSD-2-Clause)](https://packages.debian.org/sid/megasdk-dev),
  [rclone discussion of a native MEGA backend](https://forum.rclone.org/t/native-mega-backend/42175)
