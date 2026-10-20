# Plan: more clouds than Google Drive

Status: **plan**, with phase 0 partly done: engine changes 1–5 below (the `Remote` seam,
`RemoteError`, `ContentHash` with MD5, database v3, `provider` in the config and status). Still
to do in phase 0: provider-neutral strings, the keyring (6) and `account` in status.json (7; the
database already records it). Google Drive is still the only provider.

Target: OneDrive, Dropbox, MEGA, Proton Drive, and self-hosted storage (Nextcloud/ownCloud
and other WebDAV servers, S3-compatible buckets, SFTP).

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
| **Nextcloud / ownCloud** | Login Flow v2 (browser hands back an app password) | the user's own account, one folder | chunked upload v2 | `oc:checksums` (SHA1 / MD5) | versions app; keep-aside copy | `quota-*-bytes` | M |
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

## Engine changes (phase 0, no visible change)

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
| 0 (partly done, see Status) | `Remote` seam, `ContentHash`, DB v3, provider-neutral strings, `provider` in status. Google Drive only. | All current tests pass unchanged; Drive behaviour identical. |
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
