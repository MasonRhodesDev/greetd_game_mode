//! access-gate verifier: WebAuthn relying party for phone-as-identity
//! approvals of game-mode entry, plus the Web Push sender that nudges the
//! phone.
//!
//! Two planes:
//!
//! ```text
//! - WEB (tcp 127.0.0.1:AG_WEB_PORT, proxied to HTTPS by `tailscale serve`):
//!     /                      status JSON
//!     /enroll, /enroll/*     one-time passkey registration (flag-gated)
//!     /setup, /sw.js, /push/subscribe
//!                            one-time Web Push subscription (state-gated)
//!     /approve/<id>, ...     assertion ceremony deciding a request
//! - CTRL (unix socket AG_CTRL_SOCKET, 0660 owner:group of the service):
//!     newline-delimited JSON, blocking request/response. The daemon writes
//!     one request line; the verifier answers `{"id":..}` immediately and
//!     `{"status":..}` once the phone decides (or the wait times out).
//!     No polling, and unlike a localhost TCP port the socket permissions
//!     limit who can create requests at all.
//! ```
//!
//! Trust = the single enrolled passkey (phone secure element + biometric,
//! user verification required on every assertion). The push notification
//! carries no authority.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::elliptic_curve::rand_core::OsRng;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::pkcs8::DecodePrivateKey;
use p256::SecretKey;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Response, Server};
use tracing::{info, warn};
use url::Url;
use uuid::Uuid;
use web_push::{ContentEncoding, SubscriptionInfo, VapidSignatureBuilder, WebPushMessageBuilder};
use webauthn_rs::prelude::*;

struct Cfg {
    rp_id: String,
    origin: String,
    web_port: u16,
    ctrl_socket: PathBuf,
    data_dir: PathBuf,
    request_ttl: u64,
    vapid_sub: String,
    user_name: String,
}

impl Cfg {
    fn from_env() -> Result<Self> {
        let var = |k: &str| std::env::var(k).ok();
        let rp_id = var("AG_RP_ID").context("AG_RP_ID not set")?;
        let origin = var("AG_ORIGIN").context("AG_ORIGIN not set")?;
        Ok(Cfg {
            rp_id,
            origin,
            web_port: var("AG_WEB_PORT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(8730),
            ctrl_socket: var("AG_CTRL_SOCKET")
                .unwrap_or_else(|| "/run/access-gate/ctrl.sock".into())
                .into(),
            data_dir: var("AG_DATA_DIR")
                .unwrap_or_else(|| "/var/lib/access-gate".into())
                .into(),
            request_ttl: var("AG_REQUEST_TTL")
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
            vapid_sub: var("AG_VAPID_SUB").unwrap_or_else(|| "access-gate@localhost".into()),
            user_name: var("AG_USER_NAME").unwrap_or_else(|| "game-mode".into()),
        })
    }
}

struct ApprovalRequest {
    exe: String,
    path: String,
    group: String,
    status: String, // pending | approved | denied | timeout
    created: Instant,
    auth: Option<PasskeyAuthentication>,
}

struct App {
    cfg: Cfg,
    webauthn: Webauthn,
    vapid: SecretKey,
    requests: Mutex<HashMap<String, ApprovalRequest>>,
    decided: Condvar,
    enroll_state: Mutex<Option<PasskeyRegistration>>,
}

impl App {
    fn cred_file(&self) -> PathBuf {
        self.cfg.data_dir.join("credential.json")
    }
    fn sub_file(&self) -> PathBuf {
        self.cfg.data_dir.join("push_subscription.json")
    }
    fn enroll_flag(&self) -> PathBuf {
        self.cfg.data_dir.join("enroll-open")
    }
    fn push_flag(&self) -> PathBuf {
        self.cfg.data_dir.join("push-open")
    }

    fn load_passkey(&self) -> Option<Passkey> {
        let text = fs::read_to_string(self.cred_file()).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn store_passkey(&self, pk: &Passkey) -> Result<()> {
        let path = self.cred_file();
        fs::write(&path, serde_json::to_string(pk)?)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    fn enroll_allowed(&self) -> bool {
        self.enroll_flag().exists() && self.load_passkey().is_none()
    }

    fn push_setup_allowed(&self) -> bool {
        self.push_flag().exists() || !self.sub_file().exists()
    }

    fn vapid_public_b64u(&self) -> String {
        let point = self.vapid.public_key().to_encoded_point(false);
        URL_SAFE_NO_PAD.encode(point.as_bytes())
    }

    fn vapid_private_b64u(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.vapid.to_bytes())
    }
}

// ---------------------------------------------------------------------------
// VAPID key handling
// ---------------------------------------------------------------------------

/// Load the VAPID key, generating a fresh one when missing or unparsable.
/// A fresh key invalidates any existing subscription (the browser's
/// applicationServerKey no longer matches), so the subscription is dropped,
/// which re-opens the /setup page.
fn load_or_generate_vapid(data_dir: &Path) -> Result<SecretKey> {
    let key_file = data_dir.join("vapid_private.pem");
    if let Ok(pem) = fs::read_to_string(&key_file) {
        if let Ok(k) = SecretKey::from_sec1_pem(&pem) {
            return Ok(k);
        }
        if let Ok(k) = SecretKey::from_pkcs8_pem(&pem) {
            return Ok(k);
        }
        warn!("existing VAPID key unparsable; generating a new one (push subscription dropped)");
        let _ = fs::remove_file(data_dir.join("push_subscription.json"));
    }
    let key = SecretKey::random(&mut OsRng);
    let pem = key
        .to_sec1_pem(Default::default())
        .map_err(|e| anyhow!("PEM encode: {e}"))?;
    fs::write(&key_file, pem.as_bytes())?;
    fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600))?;
    info!("generated new VAPID key");
    Ok(key)
}

// ---------------------------------------------------------------------------
// Web Push (best-effort, never blocks a ceremony)
// ---------------------------------------------------------------------------

fn send_push(app: &App, payload: Value) {
    let Ok(sub_text) = fs::read_to_string(app.sub_file()) else {
        return;
    };
    let sub: SubscriptionInfo = match serde_json::from_str(&sub_text) {
        Ok(s) => s,
        Err(e) => {
            warn!("push subscription unreadable: {e}");
            return;
        }
    };
    let result = (|| -> Result<u16> {
        let mut sig = VapidSignatureBuilder::from_base64(
            &app.vapid_private_b64u(),
            web_push::URL_SAFE_NO_PAD,
            &sub,
        )
        .map_err(|e| anyhow!("vapid: {e:?}"))?;
        sig.add_claim("sub", format!("mailto:{}", app.cfg.vapid_sub));
        let signature = sig.build().map_err(|e| anyhow!("vapid build: {e:?}"))?;

        let body = payload.to_string();
        let mut msg = WebPushMessageBuilder::new(&sub);
        msg.set_payload(ContentEncoding::Aes128Gcm, body.as_bytes());
        msg.set_vapid_signature(signature);
        msg.set_ttl(app.cfg.request_ttl as u32);
        let msg = msg.build().map_err(|e| anyhow!("message build: {e:?}"))?;

        // Send manually (instead of the crate's async clients) so the request
        // carries `Urgency: high` — Android defers normal-priority pushes on
        // a dozing phone, and approvals must wake it.
        let mut req = ureq::post(&msg.endpoint.to_string())
            .timeout(Duration::from_secs(10))
            .set("TTL", &msg.ttl.to_string())
            .set("Urgency", "high");
        let resp = match msg.payload {
            Some(p) => {
                for (k, v) in &p.crypto_headers {
                    req = req.set(k, v);
                }
                req.set("Content-Encoding", "aes128gcm")
                    .send_bytes(&p.content)
            }
            None => req.call(),
        };
        match resp {
            Ok(r) => Ok(r.status()),
            Err(ureq::Error::Status(code, _)) => Ok(code),
            Err(e) => Err(anyhow!("transport: {e}")),
        }
    })();

    match result {
        Ok(code @ (200..=299)) => info!("web push sent ({code})"),
        Ok(code @ (404 | 410)) => {
            warn!("push endpoint gone ({code}); dropping subscription");
            let _ = fs::remove_file(app.sub_file());
        }
        Ok(code) => warn!("web push rejected ({code})"),
        Err(e) => warn!("web push failed: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Control plane: unix socket, blocking request/response
// ---------------------------------------------------------------------------

fn new_request_id() -> String {
    let mut bytes = [0u8; 9];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn handle_ctrl(stream: UnixStream, app: Arc<App>) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let req: Value = serde_json::from_str(line.trim()).context("bad request json")?;

    let rid = new_request_id();
    let exe = req["exe"].as_str().unwrap_or("?").to_string();
    let path = req["path"].as_str().unwrap_or("?").to_string();
    let group = req["group"].as_str().unwrap_or("?").to_string();
    let title = req["title"].as_str().unwrap_or("").to_string();
    let wait_secs = req["timeout_secs"]
        .as_u64()
        .unwrap_or(90)
        .min(app.cfg.request_ttl);

    {
        let mut requests = app.requests.lock().unwrap();
        requests.insert(
            rid.clone(),
            ApprovalRequest {
                exe: exe.clone(),
                path: path.clone(),
                group,
                status: "pending".into(),
                created: Instant::now(),
                auth: None,
            },
        );
    }
    info!("request {rid} created (exe={exe})");

    {
        let app = app.clone();
        let payload = json!({ "rid": rid, "exe": exe, "path": path, "title": title });
        thread::spawn(move || send_push(&app, payload));
    }

    let mut writer = stream;
    writer.write_all(format!("{}\n", json!({ "id": rid })).as_bytes())?;
    writer.flush()?;

    // Block until the web plane decides or the wait expires.
    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    let final_status;
    {
        let mut requests = app.requests.lock().unwrap();
        loop {
            let status = requests
                .get(&rid)
                .map(|r| r.status.clone())
                .unwrap_or_else(|| "unknown".into());
            if status != "pending" {
                final_status = status;
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                final_status = "timeout".to_string();
                break;
            }
            let (guard, _) = app.decided.wait_timeout(requests, deadline - now).unwrap();
            requests = guard;
        }
        requests.remove(&rid);
    }
    info!("request {rid}: {final_status}");
    writer.write_all(format!("{}\n", json!({ "status": final_status })).as_bytes())?;
    Ok(())
}

fn run_ctrl(app: Arc<App>) -> Result<()> {
    let sock = &app.cfg.ctrl_socket;
    if let Some(dir) = sock.parent() {
        fs::create_dir_all(dir).ok();
    }
    let _ = fs::remove_file(sock);
    let listener = UnixListener::bind(sock).with_context(|| format!("bind {sock:?}"))?;
    // group-rw: the service's group (e.g. greeter) may create requests
    fs::set_permissions(sock, fs::Permissions::from_mode(0o660))?;
    info!("control listening on {sock:?}");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let app = app.clone();
                thread::spawn(move || {
                    if let Err(e) = handle_ctrl(s, app) {
                        warn!("ctrl connection error: {e}");
                    }
                });
            }
            Err(e) => warn!("ctrl accept error: {e}"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Web plane
// ---------------------------------------------------------------------------

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn respond_json(req: tiny_http::Request, status: u16, v: Value) {
    let resp = Response::from_string(v.to_string())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"));
    let _ = req.respond(resp);
}

fn respond_html(req: tiny_http::Request, body: String) {
    let resp =
        Response::from_string(body).with_header(header("Content-Type", "text/html; charset=utf-8"));
    let _ = req.respond(resp);
}

fn respond_text(req: tiny_http::Request, status: u16, body: &str) {
    let resp = Response::from_string(body).with_status_code(status);
    let _ = req.respond(resp);
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn read_body(req: &mut tiny_http::Request) -> String {
    let mut body = String::new();
    let _ = req.as_reader().take(1 << 20).read_to_string(&mut body);
    body
}

fn gc_requests(app: &App) {
    let ttl = Duration::from_secs(app.cfg.request_ttl);
    let mut requests = app.requests.lock().unwrap();
    for r in requests.values_mut() {
        if r.status == "pending" && r.created.elapsed() > ttl {
            r.status = "timeout".into();
        }
    }
    app.decided.notify_all();
}

fn handle_web(mut req: tiny_http::Request, app: Arc<App>) {
    let method = req.method().clone();
    let url = req.url().split('?').next().unwrap_or("/").to_string();
    let parts: Vec<String> = url
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let parts_ref: Vec<&str> = parts.iter().map(String::as_str).collect();

    gc_requests(&app);

    match (&method, parts_ref.as_slice()) {
        (Method::Get, []) => respond_json(
            req,
            200,
            json!({
                "service": "access-gate-verifier",
                "rp_id": app.cfg.rp_id,
                "enrolled": app.load_passkey().is_some(),
                "push_subscribed": app.sub_file().exists(),
            }),
        ),

        // ----- enrollment (one-time, flag-gated) -----
        (Method::Get, ["enroll"]) => {
            if !app.enroll_allowed() {
                respond_text(
                    req,
                    403,
                    "Enrollment closed (a passkey is already registered, or the enroll window is not open).",
                );
                return;
            }
            respond_html(req, PAGE_ENROLL.to_string());
        }
        (Method::Post, ["enroll", "options"]) => {
            if !app.enroll_allowed() {
                respond_text(req, 404, "");
                return;
            }
            match app.webauthn.start_passkey_registration(
                Uuid::new_v4(),
                &app.cfg.user_name,
                &app.cfg.user_name,
                None,
            ) {
                Ok((ccr, state)) => {
                    *app.enroll_state.lock().unwrap() = Some(state);
                    respond_json(req, 200, serde_json::to_value(&ccr).unwrap());
                }
                Err(e) => {
                    warn!("start registration: {e}");
                    respond_text(req, 500, "registration start failed");
                }
            }
        }
        (Method::Post, ["enroll", "verify"]) => {
            if !app.enroll_allowed() {
                respond_text(req, 404, "");
                return;
            }
            let body = read_body(&mut req);
            let Some(state) = app.enroll_state.lock().unwrap().take() else {
                respond_text(req, 400, "no enrollment in progress");
                return;
            };
            let cred: RegisterPublicKeyCredential = match serde_json::from_str(&body) {
                Ok(c) => c,
                Err(e) => {
                    respond_text(req, 400, &format!("bad credential: {e}"));
                    return;
                }
            };
            match app.webauthn.finish_passkey_registration(&cred, &state) {
                Ok(passkey) => {
                    if let Err(e) = app.store_passkey(&passkey) {
                        warn!("storing passkey: {e}");
                        respond_text(req, 500, "could not store credential");
                        return;
                    }
                    let _ = fs::remove_file(app.enroll_flag());
                    info!("passkey enrolled");
                    respond_json(req, 200, json!({"ok": true}));
                }
                Err(e) => {
                    warn!("finish registration: {e}");
                    respond_text(req, 400, &format!("verification failed: {e}"));
                }
            }
        }

        // ----- push setup (one-time, state-gated) -----
        (Method::Get, ["sw.js"]) => {
            let resp = Response::from_string(SW_JS)
                .with_header(header("Content-Type", "application/javascript"));
            let _ = req.respond(resp);
        }
        (Method::Get, ["setup"]) => {
            if !app.push_setup_allowed() {
                respond_text(
                    req,
                    403,
                    &format!(
                        "Push setup closed (a subscription exists). To re-do it: touch {} on the desktop.",
                        app.push_flag().display()
                    ),
                );
                return;
            }
            respond_html(
                req,
                PAGE_SETUP.replace("__VAPID_PUB__", &app.vapid_public_b64u()),
            );
        }
        (Method::Post, ["push", "subscribe"]) => {
            if !app.push_setup_allowed() {
                respond_text(req, 404, "");
                return;
            }
            let body = read_body(&mut req);
            let v: Value = match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(_) => {
                    respond_text(req, 400, "bad subscription");
                    return;
                }
            };
            if v.get("endpoint").and_then(Value::as_str).is_none() {
                respond_text(req, 400, "bad subscription");
                return;
            }
            let path = app.sub_file();
            if fs::write(&path, v.to_string()).is_err() {
                respond_text(req, 500, "could not store subscription");
                return;
            }
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
            let _ = fs::remove_file(app.push_flag());
            info!("push subscription stored");
            respond_json(req, 200, json!({"ok": true}));
        }

        // ----- approval ceremony -----
        (Method::Get, ["approve", rid]) => {
            let requests = app.requests.lock().unwrap();
            let Some(r) = requests.get(*rid) else {
                drop(requests);
                respond_text(req, 404, "Unknown or expired request.");
                return;
            };
            let page = PAGE_APPROVE
                .replace("__RID__", rid)
                .replace("__EXE__", &html_escape(&r.exe))
                .replace("__PATH__", &html_escape(&r.path))
                .replace("__GROUP__", &html_escape(&r.group));
            drop(requests);
            respond_html(req, page);
        }
        (Method::Post, ["approve", rid, "options"]) => {
            let Some(passkey) = app.load_passkey() else {
                respond_text(req, 404, "");
                return;
            };
            let mut requests = app.requests.lock().unwrap();
            let Some(r) = requests.get_mut(*rid) else {
                drop(requests);
                respond_text(req, 404, "");
                return;
            };
            if r.status != "pending" {
                drop(requests);
                respond_text(req, 404, "");
                return;
            }
            match app.webauthn.start_passkey_authentication(&[passkey]) {
                Ok((rcr, state)) => {
                    r.auth = Some(state);
                    drop(requests);
                    respond_json(req, 200, serde_json::to_value(&rcr).unwrap());
                }
                Err(e) => {
                    drop(requests);
                    warn!("start authentication: {e}");
                    respond_text(req, 500, "authentication start failed");
                }
            }
        }
        (Method::Post, ["approve", rid, "verify"]) => {
            let body = read_body(&mut req);
            let Some(mut passkey) = app.load_passkey() else {
                respond_text(req, 404, "");
                return;
            };
            let state = {
                let mut requests = app.requests.lock().unwrap();
                match requests.get_mut(*rid) {
                    Some(r) if r.status == "pending" => r.auth.take(),
                    _ => None,
                }
            };
            let Some(state) = state else {
                respond_text(req, 404, "");
                return;
            };
            let cred: PublicKeyCredential = match serde_json::from_str(&body) {
                Ok(c) => c,
                Err(e) => {
                    respond_text(req, 400, &format!("bad credential: {e}"));
                    return;
                }
            };
            match app.webauthn.finish_passkey_authentication(&cred, &state) {
                Ok(result) => {
                    if !result.user_verified() {
                        warn!("assertion without user verification rejected");
                        respond_text(req, 403, "user verification required");
                        return;
                    }
                    if passkey.update_credential(&result).is_some() {
                        let _ = app.store_passkey(&passkey);
                    }
                    let mut requests = app.requests.lock().unwrap();
                    if let Some(r) = requests.get_mut(*rid) {
                        r.status = "approved".into();
                    }
                    app.decided.notify_all();
                    drop(requests);
                    respond_json(req, 200, json!({"ok": true}));
                }
                Err(e) => {
                    warn!("finish authentication: {e}");
                    respond_text(req, 400, &format!("verification failed: {e}"));
                }
            }
        }
        (Method::Post, ["approve", rid, "deny"]) => {
            let mut requests = app.requests.lock().unwrap();
            if let Some(r) = requests.get_mut(*rid) {
                if r.status == "pending" {
                    r.status = "denied".into();
                }
            }
            app.decided.notify_all();
            drop(requests);
            respond_json(req, 200, json!({"ok": true}));
        }

        _ => respond_text(req, 404, ""),
    }
}

// ---------------------------------------------------------------------------
// Inline pages (no external assets)
// ---------------------------------------------------------------------------

const JS_HELPERS: &str = r#"
function b64uToBuf(s){s=s.replace(/-/g,'+').replace(/_/g,'/');const p=s.length%4;if(p)s+='='.repeat(4-p);
const b=atob(s),a=new Uint8Array(b.length);for(let i=0;i<b.length;i++)a[i]=b.charCodeAt(i);return a.buffer;}
function bufToB64u(b){const a=new Uint8Array(b);let s='';for(let i=0;i<a.length;i++)s+=String.fromCharCode(a[i]);
return btoa(s).replace(/\+/g,'-').replace(/\//g,'_').replace(/=+$/,'');}
"#;

const PAGE_ENROLL_TMPL: &str = r#"<!doctype html><meta name=viewport content="width=device-width,initial-scale=1">
<title>access-gate enroll</title><body style="font-family:sans-serif;max-width:30em;margin:3em auto;padding:0 1em">
<h2>Register this phone as your game-mode key</h2>
<button id=go style="font-size:1.2em;padding:.6em 1.2em">Create passkey</button>
<p id=msg></p><script>//HELPERS//
document.getElementById('go').onclick=async()=>{
 const m=document.getElementById('msg');m.textContent='...';
 try{
  const j=await (await fetch('/enroll/options',{method:'POST'})).json();
  const o=j.publicKey;
  o.challenge=b64uToBuf(o.challenge);o.user.id=b64uToBuf(o.user.id);
  if(o.excludeCredentials)o.excludeCredentials.forEach(c=>c.id=b64uToBuf(c.id));
  const cred=await navigator.credentials.create({publicKey:o});
  const r=cred.response;
  const body={id:cred.id,rawId:bufToB64u(cred.rawId),type:cred.type,extensions:{},response:{
   attestationObject:bufToB64u(r.attestationObject),clientDataJSON:bufToB64u(r.clientDataJSON)}};
  const res=await fetch('/enroll/verify',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});
  m.textContent=res.ok?'Enrolled. You can close this.':'Verify failed: '+await res.text();
 }catch(e){m.textContent='Error: '+e;}
};</script></body>"#;

const PAGE_SETUP_TMPL: &str = r#"<!doctype html><meta name=viewport content="width=device-width,initial-scale=1">
<title>access-gate push setup</title><body style="font-family:sans-serif;max-width:30em;margin:3em auto;padding:0 1em">
<h2>Enable approval notifications on this phone</h2>
<p>One-time setup. Future approvals are: tap the notification, touch the
fingerprint sensor, done.</p>
<button id=go style="font-size:1.2em;padding:.6em 1.2em">Enable notifications</button>
<p id=msg></p><script>//HELPERS//
document.getElementById('go').onclick=async()=>{
 const m=document.getElementById('msg');m.textContent='...';
 try{
  const reg=await navigator.serviceWorker.register('/sw.js');
  await navigator.serviceWorker.ready;
  if(await Notification.requestPermission()!=='granted'){m.textContent='Notification permission denied.';return;}
  const sub=await reg.pushManager.subscribe({userVisibleOnly:true,
   applicationServerKey:b64uToBuf('__VAPID_PUB__')});
  const res=await fetch('/push/subscribe',{method:'POST',
   headers:{'content-type':'application/json'},body:JSON.stringify(sub.toJSON())});
  m.textContent=res.ok?'Subscribed. You can close this.':'Subscribe failed: '+await res.text();
 }catch(e){m.textContent='Error: '+e;}
};</script></body>"#;

const PAGE_APPROVE_TMPL: &str = r#"<!doctype html><meta name=viewport content="width=device-width,initial-scale=1">
<title>access-gate approval</title><body style="font-family:sans-serif;max-width:30em;margin:3em auto;padding:0 1em">
<h2 id=hd>Access request</h2>
<p><b>Process:</b> <code>__EXE__</code><br><b>Path:</b> <code>__PATH__</code><br><b>Group:</b> __GROUP__</p>
<p id=msg style="font-size:1.2em"></p>
<button id=ok style="font-size:1.2em;padding:.6em 1.2em;margin-right:1em;display:none">Approve</button>
<button id=no style="font-size:1.2em;padding:.6em 1.2em">Deny</button>
<script>//HELPERS//
const RID='__RID__',m=document.getElementById('msg'),ok=document.getElementById('ok'),
 no=document.getElementById('no'),hd=document.getElementById('hd');
function done(){setTimeout(()=>window.close(),1500);}
no.onclick=async()=>{await fetch('/approve/'+RID+'/deny',{method:'POST'});
 hd.textContent='Denied ✕';m.textContent='';no.style.display=ok.style.display='none';done();};
async function approve(){
 m.textContent='Confirm with your fingerprint…';
 try{
  const j=await (await fetch('/approve/'+RID+'/options',{method:'POST'})).json();
  const o=j.publicKey;
  o.challenge=b64uToBuf(o.challenge);
  if(o.allowCredentials)o.allowCredentials.forEach(c=>c.id=b64uToBuf(c.id));
  const cred=await navigator.credentials.get({publicKey:o});
  const r=cred.response;
  const body={id:cred.id,rawId:bufToB64u(cred.rawId),type:cred.type,extensions:{},response:{
   authenticatorData:bufToB64u(r.authenticatorData),clientDataJSON:bufToB64u(r.clientDataJSON),
   signature:bufToB64u(r.signature),userHandle:r.userHandle?bufToB64u(r.userHandle):null}};
  const res=await fetch('/approve/'+RID+'/verify',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});
  if(res.ok){hd.textContent='Approved ✓';m.textContent='';no.style.display=ok.style.display='none';done();}
  else{m.textContent='Verify failed: '+await res.text();}
 }catch(e){
  // Auto-fire blocked or dismissed: fall back to an explicit button.
  m.textContent='';ok.style.display='inline-block';
  ok.onclick=()=>{ok.style.display='none';approve();};
 }
}
approve();
</script></body>"#;

const SW_JS: &str = r#"
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', e => e.waitUntil(clients.claim()));
self.addEventListener('push', e => {
  let d = {};
  try { d = e.data.json(); } catch (_) {}
  e.waitUntil(self.registration.showNotification(d.title || 'Access approval needed', {
    body: (d.exe || '?') + ' → ' + (d.path || '?'),
    tag: d.rid || 'access-gate',
    requireInteraction: true,
    data: { url: '/approve/' + (d.rid || '') },
  }));
});
self.addEventListener('notificationclick', e => {
  e.notification.close();
  e.waitUntil((async () => {
    const wins = await clients.matchAll({ type: 'window', includeUncontrolled: true });
    const old = wins.find(w => new URL(w.url).pathname.startsWith('/approve/'));
    if (old) {
      try {
        await old.navigate(e.notification.data.url);
        return old.focus();
      } catch (_) { /* uncontrolled client; fall through */ }
    }
    return clients.openWindow(e.notification.data.url);
  })());
});
"#;

// Helpers are spliced into the page templates once, at first use.
fn page(tmpl: &str) -> String {
    tmpl.replace("//HELPERS//", JS_HELPERS)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .without_time()
        .init();

    let cfg = Cfg::from_env()?;
    fs::create_dir_all(&cfg.data_dir).ok();

    let rp_origin = Url::parse(&cfg.origin).context("AG_ORIGIN is not a valid URL")?;
    let webauthn = WebauthnBuilder::new(&cfg.rp_id, &rp_origin)
        .context("webauthn builder")?
        .rp_name("access-gate")
        .build()
        .context("webauthn build")?;

    let vapid = load_or_generate_vapid(&cfg.data_dir)?;

    let app = Arc::new(App {
        webauthn,
        vapid,
        requests: Mutex::new(HashMap::new()),
        decided: Condvar::new(),
        enroll_state: Mutex::new(None),
        cfg,
    });

    {
        let app = app.clone();
        thread::spawn(move || {
            if let Err(e) = run_ctrl(app) {
                tracing::error!("control plane died: {e}");
                std::process::exit(1);
            }
        });
    }

    let server =
        Server::http(("127.0.0.1", app.cfg.web_port)).map_err(|e| anyhow!("web listener: {e}"))?;
    info!("web listening on 127.0.0.1:{}", app.cfg.web_port);
    loop {
        let req = server.recv()?;
        let app = app.clone();
        thread::spawn(move || handle_web(req, app));
    }
}

// Pre-rendered pages
static PAGE_ENROLL: once_cell_lite::Lazy<String> =
    once_cell_lite::Lazy::new(|| page(PAGE_ENROLL_TMPL));
static PAGE_SETUP: once_cell_lite::Lazy<String> =
    once_cell_lite::Lazy::new(|| page(PAGE_SETUP_TMPL));
static PAGE_APPROVE: once_cell_lite::Lazy<String> =
    once_cell_lite::Lazy::new(|| page(PAGE_APPROVE_TMPL));

/// Minimal Lazy<T> (std-only) so we don't pull once_cell just for three pages.
mod once_cell_lite {
    use std::sync::OnceLock;

    pub struct Lazy<T> {
        cell: OnceLock<T>,
        init: fn() -> T,
    }

    impl<T> Lazy<T> {
        pub const fn new(init: fn() -> T) -> Self {
            Self {
                cell: OnceLock::new(),
                init,
            }
        }
    }

    impl<T> std::ops::Deref for Lazy<T> {
        type Target = T;
        fn deref(&self) -> &T {
            self.cell.get_or_init(self.init)
        }
    }
}
