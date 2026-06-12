#!/usr/bin/env python3
"""access-gate verifier: WebAuthn relying party for phone-as-identity approvals.

Two HTTP listeners share this one app:
  - WEB port  (proxied by `tailscale serve` -> https on the tailnet FQDN):
      /                     status
      /enroll, /enroll/*    one-time passkey registration (gated by a flag file)
      /setup, /sw.js, /push/subscribe
                            one-time Web Push subscription (phone notifications)
      /approve/<id>, ...    assertion ceremony that approves/denies a request
  - CTRL port (127.0.0.1 only; the root daemon's helper talks to this):
      /request              create a pending approval request -> {id}
      /result/<id>          poll decision -> {status}

The control routes are refused on the WEB port, so the tailnet can never
create or read requests — it can only answer them via a passkey signature.
Trust = the single enrolled passkey (secure element, biometric); ntfy is only
a nudge and carries no authority.
"""
import json
import os
import secrets
import threading
import time
from pathlib import Path

from flask import Flask, request, jsonify, abort, Response
from werkzeug.serving import make_server

from cryptography.hazmat.primitives import serialization
from py_vapid import Vapid, b64urlencode
from pywebpush import webpush as send_webpush, WebPushException

import webauthn
from webauthn.helpers.structs import (
    AuthenticatorSelectionCriteria,
    ResidentKeyRequirement,
    UserVerificationRequirement,
    PublicKeyCredentialDescriptor,
)

RP_ID = os.environ["AG_RP_ID"]                 # e.g. mason-desktop.tailc36203.ts.net
ORIGIN = os.environ["AG_ORIGIN"]               # e.g. https://mason-desktop.tailc36203.ts.net
RP_NAME = "access-gate"
WEB_PORT = int(os.environ.get("AG_WEB_PORT", "8730"))
CTRL_PORT = int(os.environ.get("AG_CTRL_PORT", "8731"))
REQUEST_TTL = int(os.environ.get("AG_REQUEST_TTL", "120"))

DATA_DIR = Path(os.environ.get("AG_DATA_DIR", Path.home() / ".local/share/access-gate"))
DATA_DIR.mkdir(parents=True, exist_ok=True, mode=0o700)
CRED_FILE = DATA_DIR / "credential.json"       # the one enrolled passkey
ENROLL_FLAG = DATA_DIR / "enroll-open"         # touch to open the enrollment window
PUSH_SUB_FILE = DATA_DIR / "push_subscription.json"  # the phone's Web Push subscription
PUSH_OPEN_FLAG = DATA_DIR / "push-open"        # touch to allow (re-)subscribing
VAPID_KEY_FILE = DATA_DIR / "vapid_private.pem"
PUSH_SUB = "mailto:" + os.environ.get("AG_VAPID_SUB", "access-gate@localhost")

app = Flask(__name__)

# in-memory state
_lock = threading.Lock()
_requests: dict[str, dict] = {}                # id -> {exe,path,group,status,challenge,created}
_challenges: dict[str, bytes] = {}             # transient ceremony challenges (enroll)


# ---------- helpers ----------
def _is_ctrl() -> bool:
    return request.environ.get("SERVER_PORT") == str(CTRL_PORT)


def load_cred():
    if CRED_FILE.exists():
        return json.loads(CRED_FILE.read_text())
    return None


def gc_requests():
    now = time.time()
    with _lock:
        for rid in [r for r, v in _requests.items() if now - v["created"] > REQUEST_TTL]:
            if _requests[rid]["status"] == "pending":
                _requests[rid]["status"] = "expired"


# ---------- web push (notification channel; carries no authority) ----------
def _vapid() -> Vapid:
    if not VAPID_KEY_FILE.exists():
        v = Vapid()
        v.generate_keys()
        v.save_key(str(VAPID_KEY_FILE))
        VAPID_KEY_FILE.chmod(0o600)
        return v
    return Vapid.from_file(str(VAPID_KEY_FILE))


def vapid_public_key_b64u() -> str:
    raw = _vapid().public_key.public_bytes(
        serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)
    return b64urlencode(raw)


def _push_allowed() -> bool:
    return PUSH_OPEN_FLAG.exists() or not PUSH_SUB_FILE.exists()


def send_push(payload: dict):
    """Best-effort Web Push to the subscribed phone. Never raises."""
    try:
        sub = json.loads(PUSH_SUB_FILE.read_text())
    except (FileNotFoundError, ValueError):
        return
    try:
        send_webpush(
            subscription_info=sub,
            data=json.dumps(payload),
            vapid_private_key=str(VAPID_KEY_FILE),
            vapid_claims={"sub": PUSH_SUB},
            ttl=REQUEST_TTL,
        )
    except WebPushException as e:
        # 404/410 = subscription gone; drop it so /setup reopens.
        if e.response is not None and e.response.status_code in (404, 410):
            PUSH_SUB_FILE.unlink(missing_ok=True)
        print(f"access-gate-verifier: web push failed: {e}", flush=True)
    except Exception as e:
        print(f"access-gate-verifier: web push failed: {e}", flush=True)


# ---------- control plane (daemon only) ----------
@app.post("/request")
def create_request():
    if not _is_ctrl():
        abort(404)
    body = request.get_json(force=True, silent=True) or {}
    rid = secrets.token_urlsafe(9)
    with _lock:
        _requests[rid] = {
            "exe": body.get("exe", "?"), "path": body.get("path", "?"),
            "group": body.get("group", "?"), "status": "pending",
            "created": time.time(),
        }
    threading.Thread(target=send_push, daemon=True, args=({
        "rid": rid,
        "exe": body.get("exe", "?"),
        "path": body.get("path", "?"),
        "title": body.get("title", ""),
    },)).start()
    return jsonify({"id": rid})


@app.get("/result/<rid>")
def result(rid):
    if not _is_ctrl():
        abort(404)
    gc_requests()
    with _lock:
        r = _requests.get(rid)
        return jsonify({"status": r["status"] if r else "unknown"})


# ---------- enrollment (one-time, web port, flag-gated) ----------
def _enroll_allowed() -> bool:
    return ENROLL_FLAG.exists() and load_cred() is None


@app.get("/enroll")
def enroll_page():
    if _is_ctrl():
        abort(404)
    if not _enroll_allowed():
        return "Enrollment closed (a passkey is already registered, or the "\
               "enroll window is not open).", 403
    return Response(_PAGE_ENROLL, mimetype="text/html")


@app.post("/enroll/options")
def enroll_options():
    if _is_ctrl() or not _enroll_allowed():
        abort(404)
    user_id = b"mason"
    opts = webauthn.generate_registration_options(
        rp_id=RP_ID, rp_name=RP_NAME, user_id=user_id, user_name="mason",
        authenticator_selection=AuthenticatorSelectionCriteria(
            resident_key=ResidentKeyRequirement.PREFERRED,
            user_verification=UserVerificationRequirement.REQUIRED,
        ),
    )
    with _lock:
        _challenges["enroll"] = opts.challenge
    return Response(webauthn.options_to_json(opts), mimetype="application/json")


@app.post("/enroll/verify")
def enroll_verify():
    if _is_ctrl() or not _enroll_allowed():
        abort(404)
    with _lock:
        challenge = _challenges.pop("enroll", None)
    if challenge is None:
        abort(400)
    v = webauthn.verify_registration_response(
        credential=request.get_data(as_text=True),
        expected_challenge=challenge, expected_rp_id=RP_ID,
        expected_origin=ORIGIN, require_user_verification=True,
    )
    cred = {
        "credential_id": webauthn.helpers.bytes_to_base64url(v.credential_id),
        "public_key": webauthn.helpers.bytes_to_base64url(v.credential_public_key),
        "sign_count": v.sign_count,
    }
    CRED_FILE.write_text(json.dumps(cred))
    CRED_FILE.chmod(0o600)
    try:
        ENROLL_FLAG.unlink()
    except FileNotFoundError:
        pass
    return jsonify({"ok": True})


# ---------- push setup (web port; one-time, like enrollment) ----------
@app.get("/sw.js")
def service_worker():
    if _is_ctrl():
        abort(404)
    return Response(_SW_JS, mimetype="application/javascript")


@app.get("/setup")
def setup_page():
    if _is_ctrl():
        abort(404)
    if not _push_allowed():
        return ("Push setup closed (a subscription exists). To re-do it: "
                f"touch {PUSH_OPEN_FLAG} on the desktop."), 403
    html = _PAGE_SETUP.replace("__VAPID_PUB__", vapid_public_key_b64u())
    return Response(html, mimetype="text/html")


@app.post("/push/subscribe")
def push_subscribe():
    if _is_ctrl() or not _push_allowed():
        abort(404)
    sub = request.get_json(force=True, silent=True)
    if not sub or "endpoint" not in sub:
        abort(400)
    PUSH_SUB_FILE.write_text(json.dumps(sub))
    PUSH_SUB_FILE.chmod(0o600)
    PUSH_OPEN_FLAG.unlink(missing_ok=True)
    return jsonify({"ok": True})


# ---------- approval (web port, passkey assertion) ----------
@app.get("/approve/<rid>")
def approve_page(rid):
    if _is_ctrl():
        abort(404)
    with _lock:
        r = _requests.get(rid)
    if not r:
        return "Unknown or expired request.", 404
    html = _PAGE_APPROVE.replace("__RID__", rid)\
        .replace("__EXE__", _esc(r["exe"])).replace("__PATH__", _esc(r["path"]))\
        .replace("__GROUP__", _esc(r["group"]))
    return Response(html, mimetype="text/html")


@app.post("/approve/<rid>/options")
def approve_options(rid):
    if _is_ctrl():
        abort(404)
    cred = load_cred()
    with _lock:
        r = _requests.get(rid)
        if not cred or not r or r["status"] != "pending":
            abort(404)
        opts = webauthn.generate_authentication_options(
            rp_id=RP_ID,
            allow_credentials=[PublicKeyCredentialDescriptor(
                id=webauthn.helpers.base64url_to_bytes(cred["credential_id"]))],
            user_verification=UserVerificationRequirement.REQUIRED,
        )
        r["challenge"] = opts.challenge       # bind challenge to this request
    return Response(webauthn.options_to_json(opts), mimetype="application/json")


@app.post("/approve/<rid>/verify")
def approve_verify(rid):
    if _is_ctrl():
        abort(404)
    cred = load_cred()
    with _lock:
        r = _requests.get(rid)
        challenge = r.get("challenge") if r else None
    if not cred or not r or challenge is None or r["status"] != "pending":
        abort(404)
    v = webauthn.verify_authentication_response(
        credential=request.get_data(as_text=True),
        expected_challenge=challenge, expected_rp_id=RP_ID,
        expected_origin=ORIGIN,
        credential_public_key=webauthn.helpers.base64url_to_bytes(cred["public_key"]),
        credential_current_sign_count=cred["sign_count"],
        require_user_verification=True,
    )
    cred["sign_count"] = v.new_sign_count       # monotonic clone-detection
    CRED_FILE.write_text(json.dumps(cred)); CRED_FILE.chmod(0o600)
    with _lock:
        r["status"] = "approved"
    return jsonify({"ok": True})


@app.post("/approve/<rid>/deny")
def approve_deny(rid):
    if _is_ctrl():
        abort(404)
    with _lock:
        r = _requests.get(rid)
        if r and r["status"] == "pending":
            r["status"] = "denied"
    return jsonify({"ok": True})


@app.get("/")
def index():
    return jsonify({"service": "access-gate-verifier", "rp_id": RP_ID,
                    "enrolled": load_cred() is not None,
                    "push_subscribed": PUSH_SUB_FILE.exists()})


def _esc(s: str) -> str:
    return (s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;"))


# ---------- minimal inline pages (no external assets / paths) ----------
_JS_HELPERS = """
function b64uToBuf(s){s=s.replace(/-/g,'+').replace(/_/g,'/');const p=s.length%4;if(p)s+='='.repeat(4-p);
const b=atob(s),a=new Uint8Array(b.length);for(let i=0;i<b.length;i++)a[i]=b.charCodeAt(i);return a.buffer;}
function bufToB64u(b){const a=new Uint8Array(b);let s='';for(let i=0;i<a.length;i++)s+=String.fromCharCode(a[i]);
return btoa(s).replace(/\\+/g,'-').replace(/\\//g,'_').replace(/=+$/,'');}
"""

_PAGE_ENROLL = """<!doctype html><meta name=viewport content="width=device-width,initial-scale=1">
<title>access-gate enroll</title><body style="font-family:sans-serif;max-width:30em;margin:3em auto;padding:0 1em">
<h2>Register this phone as your access-gate key</h2>
<button id=go style="font-size:1.2em;padding:.6em 1.2em">Create passkey</button>
<p id=msg></p><script>""" + _JS_HELPERS + """
document.getElementById('go').onclick=async()=>{
 const m=document.getElementById('msg');m.textContent='...';
 try{
  const o=await (await fetch('/enroll/options',{method:'POST'})).json();
  o.challenge=b64uToBuf(o.challenge);o.user.id=b64uToBuf(o.user.id);
  if(o.excludeCredentials)o.excludeCredentials.forEach(c=>c.id=b64uToBuf(c.id));
  const cred=await navigator.credentials.create({publicKey:o});
  const r=cred.response;
  const body={id:cred.id,rawId:bufToB64u(cred.rawId),type:cred.type,response:{
   attestationObject:bufToB64u(r.attestationObject),clientDataJSON:bufToB64u(r.clientDataJSON)}};
  const res=await fetch('/enroll/verify',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});
  m.textContent=res.ok?'Enrolled. You can close this.':'Verify failed: '+await res.text();
 }catch(e){m.textContent='Error: '+e;}
};</script></body>"""

_PAGE_APPROVE = """<!doctype html><meta name=viewport content="width=device-width,initial-scale=1">
<title>access-gate approval</title><body style="font-family:sans-serif;max-width:30em;margin:3em auto;padding:0 1em">
<h2 id=hd>Access request</h2>
<p><b>Process:</b> <code>__EXE__</code><br><b>Path:</b> <code>__PATH__</code><br><b>Group:</b> __GROUP__</p>
<p id=msg style="font-size:1.2em"></p>
<button id=ok style="font-size:1.2em;padding:.6em 1.2em;margin-right:1em;display:none">Approve</button>
<button id=no style="font-size:1.2em;padding:.6em 1.2em">Deny</button>
<script>""" + _JS_HELPERS + """
const RID='__RID__',m=document.getElementById('msg'),ok=document.getElementById('ok'),
 no=document.getElementById('no'),hd=document.getElementById('hd');
no.onclick=async()=>{await fetch('/approve/'+RID+'/deny',{method:'POST'});
 hd.textContent='Denied \\u2715';m.textContent='';no.style.display=ok.style.display='none';};
async function approve(){
 m.textContent='Confirm with your fingerprint\\u2026';
 try{
  const o=await (await fetch('/approve/'+RID+'/options',{method:'POST'})).json();
  o.challenge=b64uToBuf(o.challenge);
  if(o.allowCredentials)o.allowCredentials.forEach(c=>c.id=b64uToBuf(c.id));
  const cred=await navigator.credentials.get({publicKey:o});
  const r=cred.response;
  const body={id:cred.id,rawId:bufToB64u(cred.rawId),type:cred.type,response:{
   authenticatorData:bufToB64u(r.authenticatorData),clientDataJSON:bufToB64u(r.clientDataJSON),
   signature:bufToB64u(r.signature),userHandle:r.userHandle?bufToB64u(r.userHandle):null}};
  const res=await fetch('/approve/'+RID+'/verify',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});
  if(res.ok){hd.textContent='Approved \\u2713';m.textContent='';no.style.display=ok.style.display='none';}
  else{m.textContent='Verify failed: '+await res.text();}
 }catch(e){
  // Auto-fire blocked or dismissed: fall back to an explicit button.
  m.textContent='';ok.style.display='inline-block';
  ok.onclick=()=>{ok.style.display='none';approve();};
 }
}
approve();
</script></body>"""


_PAGE_SETUP = """<!doctype html><meta name=viewport content="width=device-width,initial-scale=1">
<title>access-gate push setup</title><body style="font-family:sans-serif;max-width:30em;margin:3em auto;padding:0 1em">
<h2>Enable approval notifications on this phone</h2>
<p>One-time setup. Future approvals are: tap the notification, touch the
fingerprint sensor, done.</p>
<button id=go style="font-size:1.2em;padding:.6em 1.2em">Enable notifications</button>
<p id=msg></p><script>""" + _JS_HELPERS + """
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
};</script></body>"""

_SW_JS = """
self.addEventListener('push', e => {
  let d = {};
  try { d = e.data.json(); } catch (_) {}
  e.waitUntil(self.registration.showNotification(d.title || 'Access approval needed', {
    body: (d.exe || '?') + ' \\u2192 ' + (d.path || '?'),
    tag: d.rid || 'access-gate',
    requireInteraction: true,
    data: { url: '/approve/' + (d.rid || '') },
  }));
});
self.addEventListener('notificationclick', e => {
  e.notification.close();
  e.waitUntil(clients.openWindow(e.notification.data.url));
});
"""


def _serve(port, name):
    srv = make_server("127.0.0.1", port, app, threaded=True)
    print(f"access-gate-verifier: {name} listening on 127.0.0.1:{port}", flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    t = threading.Thread(target=_serve, args=(CTRL_PORT, "control"), daemon=True)
    t.start()
    _serve(WEB_PORT, "web")
