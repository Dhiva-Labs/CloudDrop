# Plan: more clouds than Google Drive

Status: **OneDrive, Dropbox, WebDAV, Nextcloud, S3-compatible storage, SFTP and MEGA are built**
and tested against fake servers (SFTP against a real OpenSSH); none has been tried with a real
account yet. **Proton Drive is built as experimental**, through Proton's own CLI, and only builds
from source offer it. **Several clouds at once** (phase 5) is built: see
[Several clouds at once](#several-clouds-at-once). See [The providers](#the-providers) and
[Phases](#phases).

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
| **MEGA** (built, see [MEGA](#mega)) | email + password (+2FA) → session | whole account (no app folder), one folder used | upload URL + local record of confirmed chunks | none server-side (end-to-end encrypted) → verify by download (plus MEGA's MAC) | keep-aside copy (server-side); MEGA versioning too | account quota | L |
| **Proton Drive** | Proton login (SRP, 2FA, maybe a second password) → session | whole account, one folder used | via SDK (encrypted blocks) | via SDK (signed manifests; plaintext digest in encrypted attributes) or verify by download | revisions; keep-aside copy | account quota | built, experimental (below) |

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
- **MEGA.** Decided by the spike: a native Rust client of MEGA's API, not the C++ SDK. See
  [MEGA](#mega) below.
- **Proton Drive.** Proton's SDK (JavaScript and C#, with Swift and Kotlin bindings) is **not
  yet supported for third-party integrations**. Authentication isn't in it yet, and general
  availability with a new cryptographic model is targeted for end 2026 / early 2027. Proton has
  said third-party clients will eventually have to use the SDK or match its safety features.
  Community Rust ports exist but are unofficial and early. **Done instead: an experimental
  provider that runs Proton's official CLI** (built on the SDK), offered only by builds from
  source, never by store, snap or PPA builds. See [Proton Drive](#proton-drive) below.

## The providers

What each client does, how it was tested, and what a release still needs. All of them are
tested against fake servers (and SFTP against a real OpenSSH), not yet against real accounts.

### OneDrive

**Built**: `src/onedrive.rs` (Microsoft Graph v1.0, personal and work or school accounts), a fake
Graph in `tests/common/onedrive_mock.rs`, and the client's tests against it in `tests/onedrive.rs`.
`remote_for` gives it the browser sign-in's tokens (`token-onedrive.json`, from `clouddrop login`);
without a client id in the build or the config, every call asks to sign in.

- **Sign-in**: `Files.ReadWrite.AppFolder` + `offline_access` on the `common` endpoints.
  `[onedrive] client_id` (empty = the build's) and `tenant` (`common` by default; `consumers`,
  `organizations` or one organisation's tenant), whose endpoints `onedrive::oauth_endpoints` gives.
- **Folders**: `top_parent` is the app folder (`special/approot`, Apps/CloudDrop). Lookups go by
  path (`items/{parent}:/{name}`), so `find_child` returns at most one item. A recycled item
  answers 404, so `get` reports NotFound, never `trashed`.
- **Uploads**: `createUploadSession` (`replace` for an existing file, after a PATCH that names and
  places it; `fail` for a new one; `fileSize` so a file that won't fit is refused before any of
  it is sent). The session is JSON with the upload URL and the target. Fragments go to the
  upload URL without the token, at most 191 × 320 KiB (Graph wants less than 60 MiB). An
  expired or finished session is NotFound; 416 asks where to go on from. Empty files skip the
  session (one `PUT …/content`). An item without `quickXorHash` is fetched once more.
- **Keep version**: a server-side copy `<stem> (replaced YYYY-MM-DD).<ext>` (UTC date, ` (2)` and
  up when taken). The monitor URL is polled without the token until `completed`.
- **Downloads** go through `/content`, which redirects to a pre-authenticated URL that is
  followed without the token, keeping the `Range`.
- **Errors**: 401 refreshes once, then Auth; 429/503 are RateLimited (`Retry-After`); 507 or
  `quotaLimitReached` (also as an inner code) is Quota; 423 and 5xx are Transient.
- **Web links**: personal accounts only, built from the cid that starts their ids
  (`https://onedrive.live.com/?cid=…&id=…`). Work and school ids don't name the SharePoint site,
  so they get none. Check the links in the live smoke test.

**A release needs**:

- An app registration in Microsoft Entra ID for "personal Microsoft accounts and accounts in any
  organizational directory", as a **public client** (no secret, "Allow public client flows"),
  platform "Mobile and desktop applications" with the **loopback redirect** `http://127.0.0.1`
  (the host the sign-in actually uses; Entra ignores the port for loopback but not the host, so
  `http://localhost` alone fails), and
  delegated Graph permissions `Files.ReadWrite.AppFolder` and `offline_access`. Its client id
  goes into the build.
- **Publisher verification** (a verified Microsoft partner account on the registration), so work
  and school accounts see a verified publisher and can consent where their organisation only
  allows verified apps. Some organisations still need an admin to consent.
- A live smoke test with a personal and a work account: the app folder's name in other
  languages, hashes right after upload on work accounts, the copy monitor, web links.

### Dropbox

**Built** (phase 1), tested against the mock only; no real account has been used yet.

- **`src/dropbox.rs`** is an API v2 client for an *App folder* app. The app folder is
  `top_parent` (id `"root"`), and every other id is Dropbox's `id:…`, which survives renames and
  moves. Calls that only take paths (making a folder, where a move or copy goes, where a new file
  goes) look the parent's path up first. `get` makes a second call to find the parent's id.
- **Uploads** use `upload_session/start`, `append_v2` and `finish`, with the last chunk sent in
  `finish`. The session string is JSON (`session_id`, parent, name, and for a replacement the
  file's id and `rev`). `query_upload` sends an empty append at offset 0, and Dropbox's
  `incorrect_offset` gives the real one. A forgotten session, or one this client can't read, is
  NotFound.
- **Nothing in the cloud is overwritten blindly.** A replacement commits `update` with the `rev`
  seen when the upload started (after moving the file to its new name and place). If someone
  changed the file meanwhile, Dropbox keeps both and names the new one "name (1)". New files
  commit `add` with autorename, so a different file with that name stays as it is. The same
  content at the same name is no conflict, so a lost reply to `finish` doesn't duplicate the file.
- **`keep_version`** copies the file to `<stem> (replaced YYYY-MM-DD).<ext>` (UTC date) with
  `copy_v2`; a second copy on the same day becomes `… (1)`. **Moves** use `move_v2` with
  autorename. Case-only renames go through a temporary name, because `move_v2` can't do them.
- **`find_child`** asks `get_metadata` for the composed path, so matches ignore case and the
  result carries Dropbox's own spelling. A deleted item is NotFound (no trash flag).
- **Errors:** 401 → refresh once, then `Auth`; 429 (and 503 with `Retry-After`) → `RateLimited`;
  `insufficient_space` / `insufficient_quota` → `Quota`; any `…/not_found` → `NotFound`;
  `too_many_write_operations` → `RateLimited`; 5xx → `Transient`. A 404 means a wrong URL, not a
  missing file, so it is `Permanent`.
- **Other details:** `Dropbox-API-Arg` headers escape everything non-ASCII as `\uXXXX`.
  `forbidden_chars` is `\` and DEL. Names Dropbox refuses for other reasons (`disallowed_name`,
  trailing spaces) become a `Permanent` error. There are no web links (Dropbox's pages go by
  path), and quota comes from `users/get_space_usage`.
- **Config:** `[dropbox] app_key` (`client_id` is read too), plus `api_base` / `content_base`,
  which only tests change.
- **`tests/common/dropbox_mock.rs`** fakes both hosts: the API host (RPC calls, `oauth2/token`,
  `oauth2/authorize`) and the content host (uploads, downloads). It supports injected faults
  (`Status`, `RateLimited`, `LostReply`), expiring tokens and sessions, deletes on "dropbox.com"
  and a storage limit. It uses nothing from CloudDrop, so `src/dropbox.rs` includes it for its
  unit tests with `#[path]`.
- **Wiring:** `remote_for` gives it the browser sign-in's tokens (`token-dropbox.json`); without
  an app key, every call asks to sign in. The engine hashes in `DropboxContent` and rounds upload
  chunks to 4 MiB.

**What a release needs:**

1. An app in the Dropbox App Console: *Scoped access*, *App folder* (named `CloudDrop`, so the
   folder is `Apps/CloudDrop`), with the permissions `files.metadata.read`, `files.content.read`,
   `files.content.write` and `account_info.read`. Allow public clients (PKCE), so the build ships
   only the app key and no secret.
2. The redirect URI `http://localhost:53691/`: Dropbox matches it exactly, port included, so the
   sign-in always listens on that port (`dropbox::REDIRECT_PORT`).
3. **Production approval.** A new app is in development status: it can link up to 500 accounts,
   and once it reaches 50, it has two weeks to be approved for production or its linking freezes.
   Dropbox reviews only after 50 users, and checks its branding guidelines (the app name must not
   contain "Dropbox"). Plan a beta that gets there.
4. A live run of the conformance suite with `CLOUDDROP_LIVE_DROPBOX` before release. It should
   check the guesses the mock encodes: the error for committing to a deleted file's id, whether
   `move_v2` changes `rev`, and that an identical `add` isn't a conflict.

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

### S3-compatible storage

**Built**: `src/s3.rs` for Amazon S3, Backblaze B2, Wasabi, Cloudflare R2, MinIO and Garage. It
signs requests with AWS Signature Version 4 itself (no AWS SDK, no new crates). A fake S3 in
`tests/common/s3_mock.rs` checks every signature with its own implementation. The client's tests
run against it, and against AWS's published SigV4 examples, in `src/s3.rs`.

- **Settings**: `[s3] endpoint` (empty = Amazon S3 in `region`), `region` (`us-east-1` when
  empty, `auto` for R2), `bucket`, `prefix` (the folder in the bucket that holds `CloudDrop/`),
  `access_key_id`, and `addressing`: `path`, `virtual`, or empty, which means virtual-hosted for
  Amazon S3 and path-style elsewhere. The secret key is `secret_access_key` in the credentials store.
- **Ids** are keys relative to the prefix: `CloudDrop/sub/a.txt`. Folders end in `/` and have a
  zero-byte marker object (`CloudDrop/sub/`), so empty folders exist. A prefix with objects under
  it counts as a folder even without a marker. `top_parent` is `""`, and `stable_ids` is false.
- **Uploads**: a file of one part (8 MiB) is one PUT with `Content-MD5` and
  `x-amz-checksum-sha256`. Bigger files are multipart uploads. Parts are 8 MiB, or a multiple of
  8 MiB for files that would need more than 10,000 parts; chunks smaller than a part are held in
  memory until a part is full. `ListParts` says where to continue after a crash. The session is
  JSON: key, upload id, part size, and a nonce that is also written to the object, so a session
  that completed just before a crash still answers Done. Starting an upload aborts earlier
  unfinished uploads of the same key, since their parts cost storage.
- **Proof before freeing** is SHA-256. A file sent in one PUT gets it from the service
  (`x-amz-checksum-sha256`, read with `x-amz-checksum-mode: ENABLED`). Where the service doesn't
  keep checksums, or refuses the header (asked once), the proof is the SHA-256 recorded in the
  object's metadata at upload. It counts only when the MD5 recorded with it equals the object's
  ETag, which the service computes, so it is the same proof as Drive's `md5Checksum`. A multipart
  object only has a checksum of its parts' checksums, and objects under SSE-KMS or SSE-C have
  ETags that aren't MD5s. For those, `get` reports no hash, so **the engine verifies them by
  download, per object**, even though `caps.hash` is SHA-256.
- **Renames and moves** are the one place CloudDrop deletes. Every object under the old key or
  prefix is copied (CopyObject, or UploadPartCopy part by part above 5 GiB), and each copy's
  SHA-256 or ETag and size are checked against the original. The old keys are then deleted only
  when GetBucketVersioning says `Enabled` (asked at most once an hour): the delete just hides them,
  and the content stays as noncurrent versions. With no versioning, `Suspended`, a service without
  the call, or a key that may not delete, the old keys stay where they were and the log says
  "kept …". Different content already at the destination (left behind by such a rename) is kept
  aside first. An upload that also renames a file treats its old key the same way.
- **Keep version** always makes a copy, `<stem> (replaced YYYY-MM-DD).<ext>` (UTC date, ` 2` and
  up when that name is taken), versioned bucket or not, because lifecycle rules can expire
  noncurrent versions.
- **Quota**: S3 has none. `storage_quota` reports (no limit, bytes under the prefix, the same),
  added up from listings at most once an hour. Above 20,000 objects it reports (None, 0, 0)
  instead and checks again a day later, since every 1,000 objects listed is a paid request.
  Noncurrent versions and unfinished uploads aren't counted.
- **Errors**: `SignatureDoesNotMatch`, `InvalidAccessKeyId` and `ExpiredToken` are Auth (a HEAD's
  403 has no body, so a listing explains it). `SlowDown`, 429 and 503 are RateLimited
  (`Retry-After` when sent). `NoSuchKey`, `NoSuchUpload` and 404 are NotFound. A full bucket (507,
  `XMinioStorageFull`, `QuotaExceeded`, B2's "storage cap exceeded") is Quota. 5xx, and errors
  inside a 200 (CopyObject, CompleteMultipartUpload), are Transient. `NoSuchBucket`, a wrong
  region and a wrong clock are Permanent, each with a sentence.
- **Web links**: none.

**Bucket permissions**: CloudDrop needs these, on one bucket and prefix (AWS IAM shown; other
services have equivalents, e.g. B2's `listFiles`, `readFiles`, `writeFiles`, `deleteFiles`):

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {"Effect": "Allow", "Action": ["s3:ListBucket", "s3:ListBucketMultipartUploads"],
     "Resource": "arn:aws:s3:::my-bucket", "Condition": {"StringLike": {"s3:prefix": ["laptop/*"]}}},
    {"Effect": "Allow", "Action": "s3:GetBucketVersioning", "Resource": "arn:aws:s3:::my-bucket"},
    {"Effect": "Allow",
     "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject", "s3:AbortMultipartUpload",
                "s3:ListMultipartUploadParts"],
     "Resource": "arn:aws:s3:::my-bucket/laptop/*"}
  ]
}
```

`s3:DeleteObject` is only used for renames in a versioned bucket. Leave it out and renames keep
the old keys there too. `s3:GetBucketVersioning` can't take the prefix condition, so it gets a
statement of its own. A lifecycle rule that aborts incomplete multipart uploads after a week
cleans up after uploads that were never finished. R2 and Garage don't have bucket versioning, so
renames there always keep the old keys.

**Left for the live tests** (MinIO in CI, phase 2): checksum support on each service, B2's
versioning answer, and virtual-hosted addressing.

### SFTP

- **Client**: pure Rust (`russh` on the `aws-lc-rs` backend reqwest already builds, plus
  `russh-sftp`); one SSH connection per client, reconnected on the next call after a drop.
- **Config** `[sftp] host port user path key_file host_key`; the password and key passphrase are
  in `credentials.json` ("sftp"). With neither a key nor a password it signs in with ssh-agent.
- **Host key**: `host_key` is a `SHA256:…` fingerprint (`sftp::fetch_host_key` shows it). Empty:
  the first key seen is trusted and written into config.toml. A different key is refused
  (Permanent).
- **Ids** are paths relative to `path` (`""` = `path` itself), so `stable_ids: false`.
- **Uploads** write `.clouddrop-<random>.part` beside the target at the given offsets, and are
  renamed into place (`posix-rename@openssh.com`, else a temporary name). `query_upload`
  resumes 2 MiB (the most data in flight) before the partial file's end, at a 1 MiB boundary,
  since writes in flight at a cut may leave holes.
- **Never overwrites what it didn't upload**: a taken name (upload, rename, new folder) first
  moves aside as `<stem> (replaced YYYY-MM-DD).<ext>`.
- **keep_version** copies on the server with OpenSSH's `copy-data` extension (9.0+), else reads
  and writes the file through the client. No shell or exec channel is used.
- **Quota** from `statvfs@openssh.com`, else `(None, 0, 0)`. A write "Failure" with under 4 MiB
  free is `Quota`.
- **Tests** (`tests/sftp.rs`) run against a real `sshd` started as the current user, and skip
  with a note where there is none (`CLOUDDROP_REQUIRE_SSHD=1` makes that a failure).

### MEGA

Status: **built** (`src/mega.rs`), tested against a fake MEGA, **not yet tried against a real
account** (none was created for this). Needs the engine's verify-by-download for freeing space.

#### Decision: a native client of MEGA's API

MEGA encrypts everything on the client, so CloudDrop has to do MEGA's cryptography itself. The
spike compared:

| | Sign-in, 2FA, session reuse | Resume after a crash | Ranges (placeholders) | Copy (keep_version) | Build |
| --- | --- | --- | --- | --- | --- |
| MEGA's C++ SDK (BSD-2) | yes | yes | yes | yes | large C++ build to vendor for the PPA and cross-build for Windows |
| `mega` crate 0.8.0 (MIT/Apache, last release Oct 2024) | yes | no: one request per file | no: whole files | no | ~30 crates, several a second version of ours (reqwest 0.12, rand 0.8, sha2 0.10, base64 0.22, thiserror 1) |
| **Native client (chosen)** | yes | yes | yes | yes | three pure-Rust crates: `aes`, `pbkdf2`, `num-bigint` |

The native client is the dozen API commands CloudDrop needs (`us0`/`us` sign-in, `f` tree, `u`
upload URL, `p` create nodes, `a` rename, `m` move, `g` download URL, `uq` quota). It follows
MEGA's C++ SDK, which is BSD-2 and has unit-test vectors. MEGA's web client source is under a
review-only licence, so it was not used. The new crates are the versions the SFTP session's
`russh` already pulls (`aes` 0.9.3, `pbkdf2` 0.13 with `hmac` 0.13, `num-bigint` 0.5.1), and
the PPA's `crates-*.tar.xz` needs regenerating (`packaging/vendor/make-crates.sh`).

#### How it fits the contract

- **Sign-in** is a form: e-mail (`[mega] email`), password and an optional 2FA code (both
  secrets). The first connection, such as `clouddrop check`, signs in and **replaces them with the
  session**: `sid`, the account's master key and user handle, in `credentials.json` (readable
  only by you). The password is never kept. It is also dropped after a rejected sign-in, so a
  wrong password or code means running setup again. The master key is what decrypts names and
  file keys, so it is as sensitive as the session. Ending sessions on mega.nz ("log out of other
  devices") revokes it; CloudDrop then asks for sign-in. MEGA sometimes asks for a proof of work
  at sign-in (HTTP 402 with `X-Hashcash`), which the client does (a few seconds of SHA-256).
- **Capabilities:** `hash: None`, because MEGA's fingerprint (a sparse CRC plus the mtime) is no
  proof, so freeing space verifies by download, and the download also checks MEGA's own MAC.
  `pin_versions: false`, `stable_ids: true` (node handles), `resumable: true`, case-sensitive,
  no forbidden characters, `chunk_unit` 1 MiB.
- **Ids:** handles survive renames and moves. **Replacing content makes a new node** with a new
  handle (MEGA has no in-place update); `Done(meta)` carries it.
- **keep_version** makes a server-side copy, `<stem> (replaced YYYY-MM-DD).<ext>` (UTC date),
  with the same key, so nothing is uploaded again. A copy already there for that key is not made
  twice.
- **MEGA's own versioning:** the replacing upload names the old file as its previous version
  (`ov`). With versioning on (MEGA's default), the old content also stays in the file's version
  history, which counts towards storage. With it off, MEGA drops the old content on replace. The
  keep-aside copy is what CloudDrop relies on, so the setting doesn't matter for its promises.
- **Uploads:** `start_upload` gets an upload URL and picks the file's AES key and nonce. The
  session string is JSON with the URL, key, nonce, size, target and name. Each `put_chunk`
  encrypts (AES-CTR), computes MEGA's chunk MACs, and POSTs up to the last MEGA chunk boundary in
  the data (128 KiB, 256 KiB … then every 1 MiB), returning `Next(boundary)`; the engine re-reads
  the tail. The server doesn't say what it holds, so after each confirmed POST the client records
  the offset and the MAC so far in `mega-uploads/<id>.json` in the data directory: that is what
  `query_upload` answers after a crash. The last POST answers a completion token, then `p`
  creates the node. A retry of that step first looks for a node with the file's key, so a lost
  answer doesn't make a second file. Records of abandoned uploads are removed after 30 days. An
  upload server that can't continue (`-4`) or an expired URL is `NotFound`, so the engine starts
  over.
- **Downloads:** `download` decrypts, checks the MAC (a mismatch is a Permanent error) and
  returns the SHA-256. `download_range` decrypts CTR from the 16-byte block holding the start.
  Download URLs are reused for 5 minutes.
- **The tree:** `f` fetches the whole tree, cached for 30 seconds; the client's own changes update
  the cache at once, and an unknown handle fetches again, at most every 5 seconds. So changes made
  elsewhere, such as a file moved to the rubbish bin on the web, show up within 30 seconds.
  Anything under the rubbish bin is `trashed`.
- **Quota:** `uq` (limit, usage including versions, usage of the cloud drive).
- **web_url:** `https://mega.nz/fm/<handle>` for folders. Files get none: MEGA's web app has no
  address for a single file of your own, only share links.
- **Errors:** `-15` ESID and `-26` (2FA) → Auth; `-3` EAGAIN (retried twice first), `-4`, `-6`,
  `-19`, HTTP 429/509 → RateLimited; `-17`, `-24`, `-29` → Quota; `-9` ENOENT → NotFound; `-18`,
  5xx, network → Transient; anything else → Permanent, with a short sentence.

#### Not done yet

- **A real account.** Everything was checked against the SDK's sources and a fake, not MEGA
  itself. The likeliest to need a fix: the upload servers' answers (empty or `"0"`, text or
  binary token), `ov` details, the hashcash header, and `f` for very large accounts.
- **Incremental updates.** No `sc` action packets: the tree is fetched whole, which is heavy for
  accounts with hundreds of thousands of items.
- No fingerprint attribute (`c`) on uploads, so MEGA's apps can't deduplicate against them, and no
  thumbnails. Uploads don't send the SDK's `d=` checksum; they rely on HTTPS.
- Items the client can't decrypt (in folders shared with the account, or keyed with keys it
  doesn't hold) are skipped. Accounts without an RSA key pair are refused with a message.
- The master key is kept as is (file permissions only). The SDK can also wrap it with a key the
  server holds; not done.
- Chunks must be at least 1 MiB (anything smaller is a clear Permanent error, not a stall).

#### How it was tested

- **Cryptography** (`src/mega.rs` unit tests): AES-CTR and chunk MACs against the SDK's
  `CtrCrypt_RegressionVectors`; the v2 PBKDF2 keys against OpenSSL (Node and Python); the v1 key
  derivation, the MAC fold, attributes and hashcash nonces (on the SDK's test tokens) against
  independent Python ports of the SDK's code. Those last ones are self-consistency only: MEGA
  publishes no vectors for them. Also round trips, chunk boundaries, RSA (CRT) decryption and
  error mapping.
- **A fake MEGA** (`tests/common/mega_mock.rs`) does the server side with its own small crypto
  (salted sign-in, RSA-wrapped session id, hashcash, 2FA), accepts only whole MEGA chunks, and
  can decrypt what was uploaded and check its MAC independently. `tests/mega.rs` covers: sign-in
  with a proof of work, keeping only the session, session reuse and ended sessions; wrong
  password and 2FA codes; an upload resumed by a new process after a simulated crash (only the
  rest is sent); whole and ranged downloads; empty files; keep_version, then replacing (old
  content kept twice: copy and version); renames, moves, rubbish bin, missing parents; EAGAIN,
  rate limits, full storage, an upload server giving up, chunks too small; and the engine
  uploading a folder into the fake.
- Not run here: real MEGA, clippy (CI runs it), the Windows build (no new C code; the crates are
  pure Rust).

### Proton Drive

Status: **built as experimental** (`src/proton.rs`), tested against a fake of Proton's CLI,
**not yet tried against a real account** (none was signed in for this). Offered only by builds
made with `CLOUDDROP_EXPERIMENTAL_PROVIDERS=1`.

#### Proton's rules

Proton's SDK README has usage guidelines for personal projects. The SDK "is not yet ready for
third-party production use", and a third-party app must:

- use the SDK, or match its guarantees;
- call only Proton's official endpoints;
- identify itself: `x-pm-appversion: external-drive-{name}@{semver}-{channel}`;
- sync from events, with no polling or recursive walks of the drive;
- leave out Proton's branding;
- tell people: "This is a third-party application not officially supported by Proton."

A new cryptographic model is planned for the end of 2026 or early 2027.

#### Decision: run Proton's CLI

A Rust copy of Proton's cryptography wouldn't match the SDK's guarantees and would break with
the new model. Embedding the TypeScript SDK would mean shipping a JavaScript runtime. So the
client runs Proton's own CLI, `proton-drive` (MIT, built with Bun on the SDK, from
<https://proton.me/download/drive/cli>), one command per call:

- The CLI signs in **in the browser and keeps the session in the system keyring**. CloudDrop
  never sees the password or the session.
- The CLI does the cryptography and talks to Proton exactly as Proton does, from Proton's own
  binary, so the first three rules hold. Official CLI builds identify as Proton's CLI, not as
  CloudDrop: requests come from Proton's client, which CloudDrop starts.
- CloudDrop is upload-only. It never lists the drive on a timer or walks it. It lists one
  folder to find a name before creating something there, and reads one node to check it.
- The name is used only to say which service it is ("Proton Drive"), with a generic icon and no
  logo. The disclosure appears in the provider's description in both apps, under the setup
  field, and in `clouddrop login` and `clouddrop check`.

#### Gating

`ProviderInfo::experimental` marks the provider. `Provider::available()` lists experimental ones
only when the build sets `CLOUDDROP_EXPERIMENTAL_PROVIDERS=1` (read at compile time). This
covers `clouddrop providers`, `providers --json`, the setup assistants and `setup --provider`.
A config naming an experimental provider is refused when it loads, with a message saying how to
build it in.

- **Sets it:** `./install.sh`, for building from source.
- **Doesn't set it:** `debian/rules` (the PPA), `snap/snapcraft.yaml`, and the Windows
  packaging scripts (MSIX/Store). A plain `cargo build` doesn't either; add the variable to
  try it.
- CI builds without it. The tests check that Proton isn't offered then, and the client tests
  run either way, because the client code is always compiled.

#### How it fits the contract

- **Sign-in** is a form with one optional field, `cli` (the CLI's path; empty means
  `proton-drive` on PATH). `clouddrop login` (and `setup` in a terminal) runs
  `proton-drive auth login`. The apps use the form and `clouddrop check`. Before sign-in, check
  answers `needs_sign_in` with "Proton Drive sign-in needed: run `proton-drive auth login`".
  `clouddrop logout` doesn't sign the CLI out, because the CLI may be used for other things;
  `proton-drive auth logout` does.
- **Ids are node uids** (`<volume>~<link>`). The CLI resolves a path segment that is a uid to
  that node, so `/my-files/<uid>` names any node, and uids survive renames and moves.
  `stable_ids: true`. New content becomes a new revision of the same node, with the same uid.
- **Capabilities:** `hash: None`, `pin_versions: false`, `stable_ids: true`, `resumable: true`,
  case-sensitive, no forbidden characters, `chunk_unit` 256 KiB.
  - There's no hash because Proton's SHA-1 is what the uploader claims, inside encrypted
    metadata, so it proves nothing about what the server holds. Freeing space downloads the
    file instead, and the CLI decrypts and verifies it.
- **Commands:** `info`, `list`, `create-folder`, `rename`, `move`, `copy`, `upload` and
  `download`, each called as `filesystem <cmd> --json [--opt=value] -- <args>`. The `--` keeps
  names that start with `-` from being read as options.
  - Output is the SDK's `NodeEntity` as JSON. Fields used: `uid`, `parentUid`,
    `name {ok, value}`, `type`, `trashTime`, `activeRevision.claimedSize` and
    `activeRevision.claimedDigests.sha1`.
  - `move` and `copy` print `{uid, ok, error}` per item and exit 0 even when an item failed, so
    the client checks `ok`.
- **One command at a time:** every CLI process uses the same session, and two at once could
  both refresh it. The client holds a lock around each run.
- **Uploads** go through local staging, because the CLI uploads local files under their local
  name:
  1. Chunks are written to `proton-staging/<session>/<name>` in the data directory. The session
     string is JSON naming that folder, the parent and the name. `query_upload` answers from
     the staged length, so a crash resumes there.
  2. The last chunk runs `upload --file-conflict-strategy=create-new-revision`, then reads the
     result with `info <parent>/<name>`.
  3. The staged file's SHA-1 must match the one the CLI recorded. Otherwise the error is
     Transient and the staging is kept for a retry.
  4. For new content of an existing file, the upload goes to that file's parent under its
     current name, so it becomes a new revision.
  5. Staging needs as much free space as the file. It is removed once the upload is done.
- **keep_version** runs `copy --name=<stem> (replaced YYYY-MM-DD).<ext>` next to the file, adding
  " (2)" and up when the name is taken.
- **Downloads:** `download` writes into a fresh folder in `proton-downloads/`, streams the
  file into the target while hashing it (SHA-256), then deletes it. `download_range` downloads
  once and keeps that copy for the next ranges of the same file. It is replaced by the next
  file, dropped when the file gets new content, and deleted when the client goes.
- **Quota:** the CLI doesn't report storage, so `(None, 0, 0)`.
- **web_url:** none (the web app has no stable address for a uid).
- **Errors** come from the CLI's stderr, plus stdout for upload summaries:
  - "You need to login first" → Auth.
  - "not found" → NotFound.
  - quota or insufficient storage → Quota.
  - 429 or "too many requests" → RateLimited.
  - network errors and timeouts → Transient. Metadata commands get 5 minutes; uploads and
    downloads get no limit.
  - an unknown command or option → Permanent ("this version of Proton's CLI doesn't work with
    CloudDrop").
  - a missing CLI → Permanent, with the download address and the `--set cli=…` hint.

#### Not done yet

- **A real account and the real CLI.** All of the above was read from the CLI's and SDK's
  sources (October 2026) and run against a fake. The likeliest to need a fix:
  - the exact wording of quota and rate-limit errors;
  - the upload summary on stdout;
  - whether `auth login` needs a terminal.
- **Proton's review.** The CLI is Proton's, but this is still a third-party app on top of it.
  Before offering it more widely, ask Proton, and move to the SDK proper when it opens to
  third-party apps (with CloudDrop's own `external-drive-clouddrop@…` app version).
- No storage figure in `check` or the apps.
- Each call starts the CLI, which costs some time. Fine for an upload-only folder, but slow for
  thousands of tiny files.
- Names that are themselves a valid node uid would be read as a uid by the CLI. Such names are
  88–108 characters of base64 with a `~` in the middle, and CloudDrop doesn't guard against
  them.

#### How it was tested

- **A fake CLI** (`examples/fake_proton_drive.rs`) answers the same commands, options, paths
  (uid segments, `\/` escapes) and JSON shapes over a folder. It refuses unknown options and
  arguments not after `--`, and logs every call and any overlapping ones. Flag files make it
  ask for sign-in or run out of storage.
- `tests/proton.rs` covers:
  - stable ids across renames and moves;
  - names that look like options;
  - an upload resumed by a new process from its staging, with chunks resent or sent ahead;
  - empty files;
  - new revisions, and keep_version twice;
  - ranges served from one download, and the copy dropped after new content;
  - errors: missing node, name taken, failed move, full storage then a retry, signed out,
    missing CLI;
  - one command at a time.
- `tests/providers.rs` runs the whole engine against it: upload a folder, rename it, edit, free
  space (verify by download) and restore.
- Unit tests in `src/proton.rs` check node parsing (plain and `{ok, value}`-wrapped), error
  mapping, paths and names. `src/remote.rs` and `src/control.rs` check the gating.

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
- Provider logos follow each brand's guidelines (Google, Microsoft, Dropbox and MEGA publish
  them) and appear only where they name the destination. Proton Drive gets no logo: Proton's
  rules for third-party apps leave out its branding.

## Several clouds at once

Phase 5: several folders uploading to several clouds side by side, e.g. `~/CloudDrop` to Google
Drive and `~/Work` to a Nextcloud.

### Connections

A **connection** is one sync folder uploading to one provider, with that provider's settings,
sign-in and state. It is exactly what an installation was until now: one config.toml and one data
directory. An existing installation *is* the connection `default`, unchanged, with nothing to
migrate. Every other connection lives beside it:

| | Config | Data (state.db, status.json, token files, credentials.json, `mega-uploads/`, verify temp files) |
| --- | --- | --- |
| `default` | `config.toml`, as today (`CLOUDDROP_CONFIG`) | the data directory, as today (`CLOUDDROP_DATA_DIR`) |
| `<name>` | `connections/<name>/config.toml`, in the directory of `config.toml` | `connections/<name>/` in the data directory |

On Linux that's `~/.config/clouddrop/connections/work/config.toml` and
`~/.local/share/clouddrop/connections/work/`; on Windows `%APPDATA%\clouddrop\connections\work\`
and `%LOCALAPPDATA%\clouddrop\connections\work\`. With `CLOUDDROP_CONFIG=/x/config.toml`, the
connections are in `/x/connections/`, so a sandbox holds all of them.

Why a directory per connection rather than one config file with a section per connection: every
reader there is (the engine, `Ctx`, the Nautilus extension, the Windows app, an older CLI) reads one
config.toml and one state.db for one folder, and keeps doing so for each connection without a
change. The list of connections is the list of directories, so there is no index to fall out of
step with them; adding one is writing a file, removing one is deleting a directory, and a daemon
notices both by looking.

What belongs to the whole installation stays at the top of the data directory: `engine.lock` (one
daemon), `daemon.json` (its heartbeat), `daemon.stop` (a request to stop it), `tray.lock` (one
top-bar icon, which shows every connection), `app.json` (the apps' remembered choice,
`{"connection": name}`) and the Windows agent's `agent.log`.

- **Names**: 1 to 32 characters: letters, digits, spaces, `-`, `_` and `.`, starting with a letter
  or digit and not ending in a space or dot. Unique ignoring case (Windows folders ignore case).
  `default` is the default connection's; Windows device names (`con`, `nul`, `com1`…) are refused.
- **Labels**: what the apps show. A connection's name, except `default`, which shows its folder's
  name ("CloudDrop"). The apps name a connection they add after its folder ("Work").
- **Folders**: two connections may not share or nest sync folders. `connections add` and `setup
  --folder` refuse one that would; the daemon skips a connection whose folder overlaps one before it
  (default first, then by name) and logs why.
- **Order**: default first, then by name, ignoring case.
- **The apps' look** (`theme`, `accent`) is the installation's: `config set theme|accent` and the
  apps write it to every connection's config, and a new connection starts with it, so switching
  folders never changes it.
- **Proton Drive**'s sign-in belongs to Proton's CLI, not to CloudDrop, so every Proton connection
  uses the account that CLI is signed in to.

### The engine

There is still one daemon: the systemd user unit runs `clouddrop run`, the snap's `engine`
autostart runs it, the Windows agent hosts it. It runs **one engine per connection**, each the
engine as it was, with its own database, status.json, pause switch and sign-in, and nothing shared
but the process (`daemon::supervise`):

- Every 2 seconds it lists the connections. A connection that is configured and signed in gets an
  engine; until then it waits, as the snap and the Windows agent always did for setup.
- A connection whose config.toml is gone (`connections remove`) has its engine stopped. The others
  carry on.
- An engine that ends is started again: after a second when it stopped on request (meta `stop`,
  which now means "restart this connection": `reset-cloud`, `keep`, `auto-free`, `logout`), after 5
  seconds doubling to a minute when it failed (a database bound to another provider before
  `reset-cloud`, a folder that can't be created).
- `clouddrop service stop` (and Quit in the Windows tray) writes `daemon.stop`, or sends SIGTERM
  through systemd; every engine stops and the daemon exits.
- The daemon writes `daemon.json` (`{"pid", "updated_ms", "connections": [names]}`) every 2
  seconds. `service status` and `control::daemon_running()` read it (or the default connection's
  status.json, as an older daemon writes only that).
- **Notifications name the connection**: a named connection's end in " (Work)" ("Uploaded to
  Nextcloud (Work)"); the default connection's read as they always did.

### The CLI

- **`--connection NAME`** (any command). Without it: the connection whose folder holds the first
  path argument, else the one whose folder holds the current directory, else `default`. So
  `clouddrop free ~/Work/a.pdf` frees it in Work, and so does `clouddrop free a.pdf` in `~/Work`.
  The apps always pass it.
- **`clouddrop connections [--json]`** lists them:
  `[{"name", "label", "folder", "provider", "provider_name", "drive_folder", "config", "data_dir",
  "configured", "signed_in", "error"}]` (`error`: why the config can't be read, else null). A connection exists
  once its config.toml does; on a new computer the list is empty.
- **`clouddrop connections add NAME --folder DIR [--provider ID] [--set K=V]… [--secret K=V]…
  [--drive-folder N] [--client-json F]`**: `setup --connection NAME --no-login` for a name not in
  use, with a folder no other connection uses. Then `clouddrop login --connection NAME` (or `check`
  for form providers). A new Google Drive connection takes the build's client, or the one another
  connection already uses. Its cloud folder (`--drive-folder`) is named after its folder unless
  given, so two connections to one account don't share one; `setup` keeps the cloud folder name a
  connection already has (it no longer falls back to "CloudDrop").
- **`clouddrop connections remove NAME`**: deletes the connection's config.toml (the daemon stops
  its engine within 2 seconds), waits for its status.json to go (10 seconds at most), then deletes
  its config directory and data directory. The sync folder and everything in it stay, and nothing in
  the cloud is touched; what CloudDrop knew (which files are cloud-only) is forgotten. Removing
  `default` deletes config.toml and the state files of the data directory, not `connections/`.
- **`status --json`** is today's object for the selected connection, plus `"connection"` and
  `"label"`; without `--connection` (and outside other connections' folders) that's the default
  connection, as before. **`status --json --all`** is an array of those objects, one per connection
  (`{"connection", "label", "error"}` for one that can't be read).
- `setup` with a new `--connection` name creates it, like `connections add`. `logout` stops only
  that connection's engine. `service` and `providers` are for the whole installation; `run` serves
  every connection.

### Windows

- Each connection's folder is its own Cloud Files **sync root**, id `CloudDrop!<user SID>!<name>`.
  The default connection's account name stays `default`, so today's registration is kept.
- Each is in File Explorer's navigation pane under its name: "CloudDrop" for the default
  connection (as today) and "CloudDrop - Work" for the others, the way OneDrive shows
  "OneDrive - Personal".
- A removed connection's sync root is unregistered by the agent (when it stops that engine, and at
  start for any CloudDrop sync root without a connection), as on uninstall: files with their data
  become ordinary files, and placeholders of freed files, which hold no data, are removed from the
  folder; those files stay in the cloud. `clouddrop-agent --unregister` does every connection.
- The notification-area menu lists the connections, each with its status and a submenu (Open
  Folder, Open in …, Pause/Resume); the icon shows the most urgent state among them.

### The apps

The GTK app and the Windows app mirror each other:

- **Connection switcher**: with more than one connection, a list of them with each one's status
  dot (in the GTK header's start, in the Windows app's navigation pane above Home). Home, Files,
  Activity and Settings show the selected one; the choice is remembered.
- **"Add Another Folder…"** (in the switcher and in Settings) runs the setup assistant for a new
  connection: provider, sign-in, folder (one no other connection uses), cloud folder. The connection
  is named after its folder. Cancelling removes what it had written.
- **Settings › "Remove This Folder…"** asks, then runs `connections remove`: the folder and the
  cloud keep everything. The app switches to another connection, or to the setup assistant when it
  was the last.
- **Top-bar icon** (`tray/`): the most urgent state of all connections; the menu lists each
  connection (its label and status line) with Open Folder, Open in …, and Pause/Resume, then Show
  CloudDrop and Quit. With one connection the menu is as it was.
- **Nautilus** (`nautilus/`): reads every connection's config and database, so emblems, the menu and
  the CloudDrop column work in each sync folder; each item uses its folder's connection, and the
  commands it runs carry `--connection`.

## Phases

| Phase | What | Done when | Status |
| --- | --- | --- | --- |
| 0 | `Remote` seam, `ContentHash`, DB v3, provider-neutral strings, `provider` in status. Google Drive only. | All current tests pass unchanged; Drive behaviour identical. | done |
| 1 | **OneDrive** and **Dropbox**. Mock servers for both in `tests/common`, like the Drive one. Setup provider picker. | e2e suite green against all three mocks; live smoke test with real accounts. | built; live accounts still to try |
| 2 | **Self-hosted**: Nextcloud (first-class), generic WebDAV, S3-compatible, SFTP. Verify-by-download and keep-aside copy land here. | e2e against real servers in CI containers: Nextcloud, MinIO, OpenSSH. | built; tested on fakes and a real OpenSSH, not yet in CI containers |
| 3 | **MEGA** (SDK or native, from the spike). | Resume, 2FA and verify-by-download tested; PPA and Windows builds reproducible. | built (native client); live account still to try |
| 4 | **Proton Drive**, experimental, through Proton's official CLI; builds from source only. | Proton's third-party terms met; their review if they require one. | built (experimental, off in store builds); real account still to try |
| 5 | **Several clouds at once**: one engine per connection (own folder, config file and data directory, see [Several clouds at once](#several-clouds-at-once)); the apps get a connection switcher. | Two providers syncing side by side. | built: tested with Google Drive, WebDAV and S3 fakes side by side (`tests/connections.rs`); the Windows agent not yet compiled on Windows |

## Testing

- **Each client against its fake server**: `tests/onedrive.rs`, `tests/webdav.rs`, `tests/sftp.rs` (a
  real local OpenSSH), `tests/mega.rs`, and the unit tests in `src/dropbox.rs` and `src/s3.rs`.
- **The whole engine with every provider**: `tests/providers.rs` uploads a folder, renames it,
  edits a file, frees space (proved by the provider's hash, or by downloading) and downloads a file
  back, against each fake server.
- **The engine's rules for other providers on the Drive mock**: `tests/e2e.rs` with
  `Harness::with_caps` (verify by download, path ids, forbidden characters, case-only clashes).
- **Several clouds at once**: `tests/connections.rs` runs the real `clouddrop run` in a sandbox
  with connections on the fake Google Drive, WebDAV and S3 side by side: pausing one, freeing by
  path, adding one and removing one while the daemon runs, and the CLI's rules.
- **Still to do**: live smoke tests with real accounts (`CLOUDDROP_LIVE_<PROVIDER>`), and real
  Nextcloud and MinIO servers in CI.

## Open questions

- Phase 1 order: OneDrive first (more users; File Explorer shows it natively, which may confuse,
  so test that) or Dropbox first (simplest API)?
- Is "verify by download" acceptable as the default for MEGA, WebDAV and SFTP, or is a size-only
  "trust upload" preferable for very large files?

## Sources

- Proton: [Drive SDK update, January 2026](https://proton.me/blog/drive-sdk-january-2026),
  [SDK repository](https://github.com/ProtonDriveApps/sdk/),
  [rclone forum thread with Proton's statement](https://forum.rclone.org/t/proton-drive-x-rclone/53609)
- Microsoft Graph: [createUploadSession](https://learn.microsoft.com/graph/api/driveitem-createuploadsession),
  [sha1Hash deprecation on OneDrive Personal](https://devblogs.microsoft.com/microsoft365dev/deprecation-of-sha1hash-on-onedrive-personal/)
- MEGA: [megasdk packaging (BSD-2-Clause)](https://packages.debian.org/sid/megasdk-dev),
  [rclone discussion of a native MEGA backend](https://forum.rclone.org/t/native-mega-backend/42175)
