//! In-process fake of the S3 API subset CloudDrop's S3 client uses, path-style
//! (`http://127.0.0.1:port/bucket/key`). It checks every request's AWS Signature Version 4 with
//! an implementation of its own, so a signing bug in the client fails the tests. It can play a
//! bucket with or without versioning, a service that doesn't keep SHA-256 checksums (or refuses
//! the headers), ETags that aren't MD5s, a storage cap, and injected errors.
//!
//! Self-contained (nothing from `clouddrop`), so the client's own unit tests include it as well
//! (`src/s3.rs`).

// Handlers return axum responses as their errors, as handlers do.
#![allow(dead_code, clippy::result_large_err)]

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use md5::Md5;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ACCESS_KEY: &str = "AKIDMOCKEXAMPLE";
pub const SECRET: &str = "mock/Secret+Key=";
pub const REGION: &str = "mock-west-1";
pub const BUCKET: &str = "bucket";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Obj {
    pub content: Vec<u8>,
    pub content_type: String,
    /// `x-amz-meta-*`, without the prefix.
    pub meta: BTreeMap<String, String>,
    /// Without quotes: the MD5 of a single-part object, "md5-of-part-md5s-N" of a multipart one.
    pub etag: String,
    /// `x-amz-checksum-sha256` (base64); "…-N" for a multipart object.
    pub checksum: Option<String>,
}

impl Obj {
    pub fn composite(&self) -> bool {
        self.checksum.as_deref().is_some_and(|c| c.contains('-'))
    }
}

struct Upload {
    key: String,
    content_type: String,
    meta: BTreeMap<String, String>,
    /// Created with `x-amz-checksum-algorithm: SHA256`: parts carry checksums.
    checksum: bool,
    parts: BTreeMap<u32, MockPart>,
}

#[derive(Clone)]
struct MockPart {
    data: Vec<u8>,
    etag: String,
    checksum: Option<String>,
}

/// An error to answer one request with, instead of doing it.
#[derive(Clone, Debug)]
pub struct Fail {
    pub method: Method,
    /// Only requests whose query has this parameter (e.g. "partNumber"); "" for any.
    pub query: &'static str,
    pub status: u16,
    pub code: &'static str,
    pub message: &'static str,
    pub retry_after: Option<u64>,
    /// Answer 200 with the error in the body, as CopyObject and CompleteMultipartUpload can.
    pub in_200: bool,
}

impl Fail {
    pub fn new(method: Method, status: u16, code: &'static str) -> Self {
        Fail { method, query: "", status, code, message: "injected", retry_after: None, in_200: false }
    }

    pub fn on(mut self, query: &'static str) -> Self {
        self.query = query;
        self
    }

    pub fn message(mut self, message: &'static str) -> Self {
        self.message = message;
        self
    }

    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
    }

    pub fn in_200(mut self) -> Self {
        self.in_200 = true;
        self
    }
}

pub struct Mock {
    pub objects: BTreeMap<String, Obj>,
    /// What a versioned bucket keeps of replaced and deleted objects, by key.
    pub noncurrent: Vec<(String, Obj)>,
    pub versioning: bool,
    /// GetBucketVersioning answers 501 NotImplemented (as on services without versioning).
    pub no_versioning_api: bool,
    /// The service keeps and reports SHA-256 checksums (false: it ignores the headers).
    pub checksums: bool,
    /// Requests with `x-amz-checksum-*` headers get 501 NotImplemented.
    pub refuse_checksums: bool,
    /// ETags look like MD5s but aren't (as with SSE-KMS).
    pub opaque_etags: bool,
    /// Bytes the bucket may hold, counting old versions and unfinished uploads.
    pub cap: Option<usize>,
    /// The smallest part but the last (5 MiB in S3).
    pub min_part: usize,
    /// The biggest object one CopyObject copies (5 GiB in S3).
    pub copy_max: usize,
    /// Objects per ListObjectsV2 page and parts per ListParts page at most.
    pub page: usize,
    uploads: HashMap<String, Upload>,
    pub fail: VecDeque<Fail>,
    /// Every request that passed the signature check: "METHOD key?query,names".
    pub log: Vec<String>,
    pub bad_signatures: u64,
    pub deletes: u64,
    pub copies: u64,
    /// The secret the mock expects; tests change it to provoke sign-in errors.
    pub secret: String,
    pub base: String,
    next: u64,
}

impl Mock {
    fn new() -> Self {
        Mock {
            objects: BTreeMap::new(),
            noncurrent: Vec::new(),
            versioning: false,
            no_versioning_api: false,
            checksums: true,
            refuse_checksums: false,
            opaque_etags: false,
            cap: None,
            min_part: 5 << 20,
            copy_max: 5 << 30,
            page: 1000,
            uploads: HashMap::new(),
            fail: VecDeque::new(),
            log: Vec::new(),
            bad_signatures: 0,
            deletes: 0,
            copies: 0,
            secret: SECRET.to_string(),
            base: String::new(),
            next: 0,
        }
    }

    /// Puts an object as another program would (no CloudDrop metadata, no checksum).
    pub fn put(&mut self, key: &str, content: &[u8]) {
        let etag = self.etag(content);
        let obj = Obj { content: content.to_vec(), content_type: "binary/octet-stream".into(), etag, ..Default::default() };
        self.store(key, obj);
    }

    pub fn keys(&self) -> Vec<String> {
        self.objects.keys().cloned().collect()
    }

    pub fn content(&self, key: &str) -> Option<Vec<u8>> {
        self.objects.get(key).map(|o| o.content.clone())
    }

    pub fn uploads_open(&self) -> usize {
        self.uploads.len()
    }

    /// Requests whose log line starts with `prefix` ("PUT ", "DELETE backup/a").
    pub fn count(&self, prefix: &str) -> usize {
        self.log.iter().filter(|l| l.starts_with(prefix)).count()
    }

    fn etag(&self, content: &[u8]) -> String {
        if self.opaque_etags {
            hex::encode(&Sha256::digest(content)[..16])
        } else {
            hex::encode(Md5::digest(content))
        }
    }

    fn store(&mut self, key: &str, obj: Obj) {
        if let Some(old) = self.objects.insert(key.to_string(), obj)
            && self.versioning
        {
            self.noncurrent.push((key.to_string(), old));
        }
    }

    fn stored(&self) -> usize {
        self.objects.values().map(|o| o.content.len()).sum::<usize>()
            + self.noncurrent.iter().map(|(_, o)| o.content.len()).sum::<usize>()
            + self.uploads.values().flat_map(|u| u.parts.values()).map(|p| p.data.len()).sum::<usize>()
    }

    fn full(&self, adding: usize) -> bool {
        self.cap.is_some_and(|cap| self.stored() + adding > cap)
    }
}

pub type Shared = Arc<Mutex<Mock>>;

pub async fn start() -> Shared {
    let shared: Shared = Arc::new(Mutex::new(Mock::new()));
    let app = Router::new()
        .fallback(handle)
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    shared.lock().unwrap().base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    shared
}

type Query = Vec<(String, String)>;

fn q<'a>(query: &'a Query, name: &str) -> Option<&'a str> {
    query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

fn hdr<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

async fn handle(State(s): State<Shared>, method: Method, uri: Uri, h: HeaderMap, body: Bytes) -> Response {
    let mut st = s.lock().unwrap();
    let head = method == Method::HEAD;
    if let Err((status, code, message)) = verify(&st.secret, &method, &uri, &h, &body) {
        st.bad_signatures += 1;
        return error(head, status, code, &message, None);
    }
    let path = decode(uri.path());
    let rest = path.strip_prefix('/').unwrap_or(&path);
    let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
    let query = parse_query(uri.query().unwrap_or(""));
    let names: Vec<&str> = query.iter().map(|(k, _)| k.as_str()).collect();
    st.log.push(format!("{method} {key}{}{}", if names.is_empty() { "" } else { "?" }, names.join(",")));
    if bucket != BUCKET {
        return error(head, 404, "NoSuchBucket", "The specified bucket does not exist", None);
    }
    if let Some(i) =
        st.fail.iter().position(|f| f.method == method && (f.query.is_empty() || q(&query, f.query).is_some()))
    {
        let f = st.fail.remove(i).unwrap();
        if f.in_200 {
            return xml(StatusCode::OK, error_xml(f.code, f.message));
        }
        return error(head, f.status, f.code, f.message, f.retry_after);
    }
    let key = key.to_string();
    let st = &mut *st;
    match method {
        Method::GET if key.is_empty() && q(&query, "list-type").is_some() => list_objects(st, &query),
        Method::GET if key.is_empty() && q(&query, "versioning").is_some() => get_versioning(st),
        Method::GET if key.is_empty() && q(&query, "uploads").is_some() => list_uploads(st, &query),
        Method::HEAD if !key.is_empty() => head_object(st, &key, &h),
        Method::GET if !key.is_empty() && q(&query, "uploadId").is_some() => list_parts(st, &query),
        Method::GET if !key.is_empty() => get_object(st, &key, &h),
        Method::PUT if !key.is_empty() && q(&query, "partNumber").is_some() => upload_part(st, &query, &h, &body),
        Method::PUT if !key.is_empty() && h.contains_key("x-amz-copy-source") => copy_object(st, &key, &h),
        Method::PUT if !key.is_empty() => put_object(st, &key, &h, &body),
        Method::POST if !key.is_empty() && q(&query, "uploads").is_some() => create_upload(st, &key, &h),
        Method::POST if !key.is_empty() && q(&query, "uploadId").is_some() => complete(st, &key, &query, &body),
        Method::DELETE if !key.is_empty() && q(&query, "uploadId").is_some() => abort(st, &query),
        Method::DELETE if !key.is_empty() => delete_object(st, &key),
        _ => error(head, 501, "NotImplemented", "The mock does not do this", None),
    }
}

// ---------- operations ----------

fn head_object(st: &Mock, key: &str, h: &HeaderMap) -> Response {
    let Some(o) = st.objects.get(key) else {
        return error(true, 404, "NoSuchKey", "", None);
    };
    let mut r = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, o.content.len())
        .header(header::ETAG, format!("\"{}\"", o.etag))
        .header(header::CONTENT_TYPE, &o.content_type);
    for (k, v) in &o.meta {
        r = r.header(format!("x-amz-meta-{k}"), v);
    }
    if hdr(h, "x-amz-checksum-mode") == Some("ENABLED")
        && let Some(c) = &o.checksum
    {
        r = r
            .header("x-amz-checksum-sha256", c)
            .header("x-amz-checksum-type", if o.composite() { "COMPOSITE" } else { "FULL_OBJECT" });
    }
    r.body(Body::empty()).unwrap()
}

fn get_object(st: &Mock, key: &str, h: &HeaderMap) -> Response {
    let Some(o) = st.objects.get(key) else {
        return error(false, 404, "NoSuchKey", "The specified key does not exist.", None);
    };
    let Some(range) = hdr(h, "range").and_then(|r| r.strip_prefix("bytes=")) else {
        return (StatusCode::OK, [(header::ETAG, format!("\"{}\"", o.etag))], o.content.clone()).into_response();
    };
    let (a, b) = range.split_once('-').unwrap();
    let a: usize = a.parse().unwrap();
    let b: usize = b.parse::<usize>().unwrap_or(usize::MAX).min(o.content.len().saturating_sub(1));
    if a >= o.content.len() {
        return error(false, 416, "InvalidRange", "The requested range is not satisfiable", None);
    }
    let cr = format!("bytes {a}-{b}/{}", o.content.len());
    (StatusCode::PARTIAL_CONTENT, [(header::CONTENT_RANGE, cr)], o.content[a..=b].to_vec()).into_response()
}

/// Checks Content-MD5 and x-amz-checksum-sha256; Ok(the checksum to keep).
fn check_body(st: &Mock, h: &HeaderMap, body: &[u8]) -> Result<Option<String>, Response> {
    if let Some(md5) = hdr(h, "content-md5")
        && md5 != B64.encode(Md5::digest(body))
    {
        return Err(error(false, 400, "BadDigest", "The Content-MD5 you specified did not match what we received.", None));
    }
    match hdr(h, "x-amz-checksum-sha256") {
        Some(c) if c != B64.encode(Sha256::digest(body)) => {
            Err(error(false, 400, "BadDigest", "The SHA256 you specified did not match the calculated checksum.", None))
        }
        Some(c) if st.checksums => Ok(Some(c.to_string())),
        _ => Ok(None),
    }
}

fn refuses(st: &Mock, h: &HeaderMap) -> Option<Response> {
    let sent = h.keys().any(|k| k.as_str().starts_with("x-amz-checksum-") && k.as_str() != "x-amz-checksum-mode");
    (st.refuse_checksums && sent).then(|| {
        error(false, 501, "NotImplemented", "A header you provided implies functionality that is not implemented", None)
    })
}

fn storage_full() -> Response {
    error(false, 403, "AccessDenied", "Cannot upload files, storage cap exceeded.", None)
}

fn meta_of(h: &HeaderMap) -> BTreeMap<String, String> {
    h.iter()
        .filter_map(|(k, v)| Some((k.as_str().strip_prefix("x-amz-meta-")?.to_string(), v.to_str().ok()?.to_string())))
        .collect()
}

fn put_object(st: &mut Mock, key: &str, h: &HeaderMap, body: &[u8]) -> Response {
    if let Some(r) = refuses(st, h) {
        return r;
    }
    let checksum = match check_body(st, h, body) {
        Ok(c) => c,
        Err(r) => return r,
    };
    if st.full(body.len()) {
        return storage_full();
    }
    let obj = Obj {
        content: body.to_vec(),
        content_type: hdr(h, "content-type").unwrap_or("binary/octet-stream").to_string(),
        meta: meta_of(h),
        etag: st.etag(body),
        checksum: checksum.clone(),
    };
    let etag = format!("\"{}\"", obj.etag);
    st.store(key, obj);
    let mut r = Response::builder().status(StatusCode::OK).header(header::ETAG, etag);
    if let Some(c) = checksum {
        r = r.header("x-amz-checksum-sha256", c);
    }
    r.body(Body::empty()).unwrap()
}

fn copy_source<'a>(st: &'a Mock, h: &HeaderMap) -> Result<&'a Obj, Response> {
    let source = hdr(h, "x-amz-copy-source").unwrap_or_default();
    let source = source.strip_prefix('/').unwrap_or(source);
    let (bucket, key) = source.split_once('/').unwrap_or((source, ""));
    match st.objects.get(&decode(key)) {
        Some(o) if bucket == BUCKET => Ok(o),
        _ => Err(error(false, 404, "NoSuchKey", "The specified key does not exist.", None)),
    }
}

fn copy_object(st: &mut Mock, key: &str, h: &HeaderMap) -> Response {
    if let Some(r) = refuses(st, h) {
        return r;
    }
    let src = match copy_source(st, h) {
        Ok(o) => o.clone(),
        Err(r) => return r,
    };
    if src.content.len() > st.copy_max {
        return error(
            false,
            400,
            "InvalidRequest",
            "The specified copy source is larger than the maximum allowable size for a copy source",
            None,
        );
    }
    if st.full(src.content.len()) {
        return storage_full();
    }
    let (content_type, meta) = if hdr(h, "x-amz-metadata-directive") == Some("REPLACE") {
        (hdr(h, "content-type").unwrap_or("binary/octet-stream").to_string(), meta_of(h))
    } else {
        (src.content_type.clone(), src.meta.clone())
    };
    let checksum = if hdr(h, "x-amz-checksum-algorithm") == Some("SHA256") && st.checksums {
        Some(B64.encode(Sha256::digest(&src.content)))
    } else {
        src.checksum.clone().filter(|_| !src.composite())
    };
    let obj = Obj { etag: st.etag(&src.content), content: src.content, content_type, meta, checksum: checksum.clone() };
    let etag = obj.etag.clone();
    st.store(key, obj);
    st.copies += 1;
    let sum = checksum.map(|c| format!("<ChecksumSHA256>{c}</ChecksumSHA256>")).unwrap_or_default();
    xml(
        StatusCode::OK,
        format!(
            "<CopyObjectResult><LastModified>2026-10-21T00:00:00.000Z</LastModified><ETag>&quot;{etag}&quot;</ETag>{sum}</CopyObjectResult>"
        ),
    )
}

fn create_upload(st: &mut Mock, key: &str, h: &HeaderMap) -> Response {
    if let Some(r) = refuses(st, h) {
        return r;
    }
    st.next += 1;
    let id = format!("upload-{}", st.next);
    let upload = Upload {
        key: key.to_string(),
        content_type: hdr(h, "content-type").unwrap_or("binary/octet-stream").to_string(),
        meta: meta_of(h),
        checksum: st.checksums && hdr(h, "x-amz-checksum-algorithm") == Some("SHA256"),
        parts: BTreeMap::new(),
    };
    st.uploads.insert(id.clone(), upload);
    xml(
        StatusCode::OK,
        format!(
            "<InitiateMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{}</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>",
            escape(key)
        ),
    )
}

fn no_upload() -> Response {
    error(false, 404, "NoSuchUpload", "The specified upload does not exist.", None)
}

fn upload_part(st: &mut Mock, query: &Query, h: &HeaderMap, body: &[u8]) -> Response {
    let id = q(query, "uploadId").unwrap_or_default().to_string();
    let number: u32 = q(query, "partNumber").and_then(|n| n.parse().ok()).unwrap_or(0);
    let Some(upload_checksum) = st.uploads.get(&id).map(|u| u.checksum) else {
        return no_upload();
    };
    let copying = h.contains_key("x-amz-copy-source");
    let data = if copying {
        let src = match copy_source(st, h) {
            Ok(o) => o,
            Err(r) => return r,
        };
        let range = hdr(h, "x-amz-copy-source-range").and_then(|r| r.strip_prefix("bytes=")).unwrap_or_default();
        let (a, b) = range.split_once('-').unwrap();
        let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
        if b >= src.content.len() || a > b {
            return error(false, 400, "InvalidArgument", "The x-amz-copy-source-range value is not valid", None);
        }
        src.content[a..=b].to_vec()
    } else {
        if st.checksums && !upload_checksum && h.contains_key("x-amz-checksum-sha256") {
            return error(false, 400, "InvalidRequest", "Checksum Type mismatch occurred, expected checksum Type: null", None);
        }
        if let Err(r) = check_body(st, h, body) {
            return r;
        }
        body.to_vec()
    };
    if st.full(data.len()) {
        return storage_full();
    }
    let checksum = upload_checksum.then(|| B64.encode(Sha256::digest(&data)));
    let etag = st.etag(&data);
    let part = MockPart { data, etag: etag.clone(), checksum: checksum.clone() };
    st.uploads.get_mut(&id).unwrap().parts.insert(number, part);
    if copying {
        let sum = checksum.map(|c| format!("<ChecksumSHA256>{c}</ChecksumSHA256>")).unwrap_or_default();
        return xml(StatusCode::OK, format!("<CopyPartResult><ETag>&quot;{etag}&quot;</ETag>{sum}</CopyPartResult>"));
    }
    let mut r = Response::builder().status(StatusCode::OK).header(header::ETAG, format!("\"{etag}\""));
    if let Some(c) = checksum {
        r = r.header("x-amz-checksum-sha256", c);
    }
    r.body(Body::empty()).unwrap()
}

fn list_parts(st: &Mock, query: &Query) -> Response {
    let Some(upload) = st.uploads.get(q(query, "uploadId").unwrap_or_default()) else {
        return no_upload();
    };
    let marker: u32 = q(query, "part-number-marker").and_then(|n| n.parse().ok()).unwrap_or(0);
    let max = q(query, "max-parts").and_then(|n| n.parse().ok()).unwrap_or(1000usize).min(st.page);
    let after: Vec<(&u32, &MockPart)> = upload.parts.range(marker + 1..).collect();
    let page = &after[..after.len().min(max)];
    let mut out = format!(
        "<ListPartsResult><Bucket>{BUCKET}</Bucket><Key>{}</Key><PartNumberMarker>{marker}</PartNumberMarker><MaxParts>{max}</MaxParts><IsTruncated>{}</IsTruncated>",
        escape(&upload.key),
        after.len() > page.len()
    );
    if let Some((n, _)) = page.last() {
        out.push_str(&format!("<NextPartNumberMarker>{n}</NextPartNumberMarker>"));
    }
    for (n, p) in page {
        out.push_str(&format!(
            "<Part><PartNumber>{n}</PartNumber><LastModified>2026-10-21T00:00:00.000Z</LastModified><ETag>&quot;{}&quot;</ETag><Size>{}</Size>",
            p.etag,
            p.data.len()
        ));
        if let Some(c) = &p.checksum {
            out.push_str(&format!("<ChecksumSHA256>{c}</ChecksumSHA256>"));
        }
        out.push_str("</Part>");
    }
    out.push_str("</ListPartsResult>");
    xml(StatusCode::OK, out)
}

fn complete(st: &mut Mock, key: &str, query: &Query, body: &[u8]) -> Response {
    let id = q(query, "uploadId").unwrap_or_default().to_string();
    let Some(upload) = st.uploads.get(&id) else {
        return no_upload();
    };
    let body = String::from_utf8_lossy(body);
    let wanted: Vec<(u32, String, Option<String>)> = elements(&body, "Part")
        .into_iter()
        .map(|p| {
            let number = element(p, "PartNumber").and_then(|n| n.trim().parse().ok()).unwrap_or(0);
            let etag = unescape(element(p, "ETag").unwrap_or_default()).trim_matches('"').to_string();
            (number, etag, element(p, "ChecksumSHA256").map(unescape))
        })
        .collect();
    let invalid = |m: &str| error(false, 400, "InvalidPart", m, None);
    if wanted.is_empty() || wanted.windows(2).any(|w| w[0].0 >= w[1].0) {
        return error(false, 400, "InvalidPartOrder", "The list of parts was not in ascending order.", None);
    }
    let mut content = Vec::new();
    let (mut md5s, mut shas) = (Vec::new(), Vec::new());
    for (i, (number, etag, sum)) in wanted.iter().enumerate() {
        let Some(part) = upload.parts.get(number) else {
            return invalid("One or more of the specified parts could not be found.");
        };
        if &part.etag != etag {
            return invalid("The part's entity tag did not match.");
        }
        if upload.checksum && sum.as_ref() != part.checksum.as_ref() {
            return invalid("The part's checksum did not match.");
        }
        if i + 1 < wanted.len() && part.data.len() < st.min_part {
            return error(false, 400, "EntityTooSmall", "Your proposed upload is smaller than the minimum allowed size", None);
        }
        content.extend_from_slice(&part.data);
        md5s.extend_from_slice(&Md5::digest(&part.data));
        shas.extend_from_slice(&Sha256::digest(&part.data));
    }
    let n = wanted.len();
    let etag = if st.opaque_etags { st.etag(&content) } else { format!("{}-{n}", hex::encode(Md5::digest(&md5s))) };
    let checksum = upload.checksum.then(|| format!("{}-{n}", B64.encode(Sha256::digest(&shas))));
    let obj = Obj { content, content_type: upload.content_type.clone(), meta: upload.meta.clone(), etag: etag.clone(), checksum };
    st.uploads.remove(&id);
    st.store(key, obj);
    xml(
        StatusCode::OK,
        format!(
            "<CompleteMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{}</Key><ETag>&quot;{etag}&quot;</ETag></CompleteMultipartUploadResult>",
            escape(key)
        ),
    )
}

fn abort(st: &mut Mock, query: &Query) -> Response {
    match st.uploads.remove(q(query, "uploadId").unwrap_or_default()) {
        Some(_) => StatusCode::NO_CONTENT.into_response(),
        None => no_upload(),
    }
}

fn delete_object(st: &mut Mock, key: &str) -> Response {
    if let Some(old) = st.objects.remove(key) {
        st.deletes += 1;
        if st.versioning {
            st.noncurrent.push((key.to_string(), old));
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

fn list_objects(st: &Mock, query: &Query) -> Response {
    let prefix = q(query, "prefix").unwrap_or_default();
    let max = q(query, "max-keys").and_then(|n| n.parse().ok()).unwrap_or(1000usize).min(st.page);
    let url = q(query, "encoding-type") == Some("url");
    let after = q(query, "continuation-token").map(|t| String::from_utf8(hex::decode(t).unwrap()).unwrap());
    let matching: Vec<(&String, &Obj)> = st
        .objects
        .iter()
        .filter(|(k, _)| k.starts_with(prefix) && after.as_ref().is_none_or(|a| *k > a))
        .collect();
    let page = &matching[..matching.len().min(max)];
    let truncated = matching.len() > page.len();
    let enc = |s: &str| if url { form_encode(s) } else { escape(s) };
    let mut out = format!(
        "<ListBucketResult><Name>{BUCKET}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{max}</MaxKeys><IsTruncated>{truncated}</IsTruncated>",
        enc(prefix),
        page.len()
    );
    if url {
        out.push_str("<EncodingType>url</EncodingType>");
    }
    if truncated && let Some((k, _)) = page.last() {
        out.push_str(&format!("<NextContinuationToken>{}</NextContinuationToken>", hex::encode(k)));
    }
    for (k, o) in page {
        out.push_str(&format!(
            "<Contents><Key>{}</Key><LastModified>2026-10-21T00:00:00.000Z</LastModified><ETag>&quot;{}&quot;</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
            enc(k),
            o.etag,
            o.content.len()
        ));
    }
    out.push_str("</ListBucketResult>");
    xml(StatusCode::OK, out)
}

fn get_versioning(st: &Mock) -> Response {
    if st.no_versioning_api {
        return error(false, 501, "NotImplemented", "A header you provided implies functionality that is not implemented", None);
    }
    let status = if st.versioning { "<Status>Enabled</Status>" } else { "" };
    xml(
        StatusCode::OK,
        format!("<VersioningConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{status}</VersioningConfiguration>"),
    )
}

fn list_uploads(st: &Mock, query: &Query) -> Response {
    let prefix = q(query, "prefix").unwrap_or_default();
    let url = q(query, "encoding-type") == Some("url");
    let mut out = format!("<ListMultipartUploadsResult><Bucket>{BUCKET}</Bucket>");
    if url {
        out.push_str("<EncodingType>url</EncodingType>");
    }
    let mut ids: Vec<(&String, &Upload)> = st.uploads.iter().filter(|(_, u)| u.key.starts_with(prefix)).collect();
    ids.sort_by(|a, b| a.0.cmp(b.0));
    for (id, u) in ids {
        let key = if url { form_encode(&u.key) } else { escape(&u.key) };
        out.push_str(&format!("<Upload><Key>{key}</Key><UploadId>{id}</UploadId></Upload>"));
    }
    out.push_str("</ListMultipartUploadsResult>");
    xml(StatusCode::OK, out)
}

// ---------- answers ----------

fn xml(status: StatusCode, body: String) -> Response {
    let body = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{body}");
    (status, [(header::CONTENT_TYPE, "application/xml")], body).into_response()
}

fn error_xml(code: &str, message: &str) -> String {
    format!("<Error><Code>{code}</Code><Message>{}</Message><RequestId>mock</RequestId></Error>", escape(message))
}

/// S3's error answer; HEAD gets the status alone.
fn error(head: bool, status: u16, code: &str, message: &str, retry_after: Option<u64>) -> Response {
    let status = StatusCode::from_u16(status).unwrap();
    let mut r = if head { status.into_response() } else { xml(status, error_xml(code, message)) };
    if let Some(s) = retry_after {
        r.headers_mut().insert(header::RETRY_AFTER, s.to_string().parse().unwrap());
    }
    r
}

// ---------- Signature Version 4, checked independently of the client ----------

type Refusal = (u16, &'static str, String);

fn verify(secret: &str, method: &Method, uri: &Uri, h: &HeaderMap, body: &[u8]) -> Result<(), Refusal> {
    let deny = |status, code, m: &str| (status, code, m.to_string());
    let auth = hdr(h, "authorization").ok_or_else(|| deny(403, "AccessDenied", "no Authorization header"))?;
    let fields = auth
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .ok_or_else(|| deny(400, "InvalidRequest", "only AWS4-HMAC-SHA256 is supported"))?;
    let field = |name: &str| {
        fields.split(',').map(str::trim).find_map(|f| f.strip_prefix(name)?.strip_prefix('='))
    };
    let malformed = || deny(400, "AuthorizationHeaderMalformed", "The authorization header is malformed");
    let (cred, signed, signature) =
        (field("Credential").ok_or_else(malformed)?, field("SignedHeaders").ok_or_else(malformed)?, field("Signature").ok_or_else(malformed)?);
    let scope: Vec<&str> = cred.split('/').collect();
    if scope.len() != 5 || scope[4] != "aws4_request" {
        return Err(malformed());
    }
    if scope[0] != ACCESS_KEY {
        return Err(deny(403, "InvalidAccessKeyId", "The AWS Access Key Id you provided does not exist in our records."));
    }
    if scope[2] != REGION || scope[3] != "s3" {
        return Err(deny(400, "AuthorizationHeaderMalformed", "the region or service is wrong"));
    }
    let date = hdr(h, "x-amz-date").ok_or_else(|| deny(403, "AccessDenied", "x-amz-date is missing"))?;
    let when = parse_amz_date(date).ok_or_else(|| deny(403, "AccessDenied", "x-amz-date is not a date"))?;
    if !date.starts_with(scope[1]) {
        return Err(deny(403, "SignatureDoesNotMatch", "the credential's date is not the request's"));
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    if (now - when).abs() > 15 * 60 {
        return Err(deny(403, "RequestTimeTooSkewed", "The difference between the request time and the current time is too large."));
    }
    let payload = hdr(h, "x-amz-content-sha256").ok_or_else(|| deny(400, "InvalidRequest", "x-amz-content-sha256 is missing"))?;
    if payload != "UNSIGNED-PAYLOAD" && payload != hex::encode(Sha256::digest(body)) {
        return Err(deny(400, "XAmzContentSHA256Mismatch", "The provided 'x-amz-content-sha256' header does not match what was computed."));
    }
    let names: Vec<&str> = signed.split(';').collect();
    for required in ["host", "x-amz-date", "x-amz-content-sha256"] {
        if !names.contains(&required) {
            return Err(deny(403, "AccessDenied", "host, x-amz-date and x-amz-content-sha256 must be signed"));
        }
    }
    if h.keys().any(|k| k.as_str().starts_with("x-amz-") && !names.contains(&k.as_str())) {
        return Err(deny(403, "AccessDenied", "every x-amz-* header must be signed"));
    }
    if names.windows(2).any(|w| w[0] >= w[1]) {
        return Err(deny(403, "SignatureDoesNotMatch", "signed headers are not sorted"));
    }
    let canonical_uri = encode(&decode(uri.path()), true);
    let mut pairs: Vec<(String, String)> = parse_query(uri.query().unwrap_or(""))
        .into_iter()
        .map(|(k, v)| (encode(&k, false), encode(&v, false)))
        .collect();
    pairs.sort();
    let canonical_query = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let mut canonical_headers = String::new();
    for n in &names {
        let values: Vec<String> = h
            .get_all(*n)
            .iter()
            .map(|v| v.to_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        canonical_headers.push_str(&format!("{n}:{}\n", values.join(",")));
    }
    let request = format!("{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed}\n{payload}");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date}\n{}/{}/{}/aws4_request\n{}",
        scope[1],
        scope[2],
        scope[3],
        hex::encode(Sha256::digest(request.as_bytes()))
    );
    let mut key = hmac(format!("AWS4{secret}").as_bytes(), scope[1].as_bytes());
    for part in [scope[2], scope[3], "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    if hex::encode(hmac(&key, to_sign.as_bytes())) != signature {
        return Err(deny(
            403,
            "SignatureDoesNotMatch",
            "The request signature we calculated does not match the signature you provided.",
        ));
    }
    Ok(())
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut k = if key.len() > 64 { Sha256::digest(key).to_vec() } else { key.to_vec() };
    k.resize(64, 0);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::digest([ipad.as_slice(), data].concat());
    Sha256::digest([opad.as_slice(), inner.as_slice()].concat()).to_vec()
}

/// Seconds since 1970 of "20130524T000000Z".
fn parse_amz_date(s: &str) -> Option<i64> {
    if s.len() != 16 || &s[8..9] != "T" || !s.ends_with('Z') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(4..6)?, n(6..8)?);
    let (hh, mm, ss) = (n(9..11)?, n(11..13)?, n(13..15)?);
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// RFC 3986 encoding as SigV4 wants it.
fn encode(s: &str, keep_slash: bool) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b'/' if keep_slash => "/".to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// Keys as Amazon S3 writes them with encoding-type=url: "+" for spaces.
fn form_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            b' ' => "+".to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// %XX decoding ("+" stays "+").
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(raw: &str) -> Query {
    raw.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn elements<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let body = &rest[i + open.len()..];
        let Some(end) = body.find(&close) else { break };
        out.push(&body[..end]);
        rest = &body[end + close.len()..];
    }
    out
}

fn element<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    elements(xml, name).into_iter().next()
}
