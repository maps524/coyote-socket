//! The page that gets the CA onto a phone, and tells the user whether it worked.
//!
//! ## Why this endpoint is not gated, when everything else is
//!
//! `/install` and `/ca.crt` are reachable without a pairing token, deliberately.
//! Stated plainly so nobody later "fixes" it:
//!
//! - It is the **only** thing the phone can reach before it can authorize
//!   anything. Requiring authorization to obtain the means of connecting
//!   securely is a deadlock, not a control.
//! - It serves exactly one thing: the CA's **public** certificate. That is not
//!   a secret under any threat model — it is the artefact we are asking the
//!   user to publish to their own device. Withholding it protects nothing.
//!
//! Gating it would cost the user the entire flow and cost an attacker nothing.
//!
//! ## What installing this certificate actually grants
//!
//! Said on the page itself rather than buried here, because the person making
//! the decision is the one holding the phone. A trusted root can vouch for
//! **any** website, not just this one. The protections are that the key is
//! generated on the user's own machine and never leaves it, and that the
//! certificate is named so it can be found and removed later. The page says
//! both, and says how to remove it.
//!
//! ## The step everyone misses
//!
//! On iOS, installing a configuration profile does **not** trust it. The
//! certificate sits installed and inert until the user separately visits
//! Settings → General → About → Certificate Trust Settings and enables full
//! trust for the root. Skipping it fails in a way indistinguishable from a
//! broken certificate: the profile is visibly installed, and HTTPS still does
//! not work.
//!
//! That cannot be automated, so the page does the next best thing — it carries
//! the numbered path, and a **check button that answers the question**. The
//! check is two-stage on purpose, because "it didn't work" has two completely
//! different causes and the same symptom:
//!
//! 1. Reach the bridge over plain HTTP by name. Failure here means the *name*
//!    did not resolve — multicast blocked, wrong network — and no amount of
//!    certificate fiddling will help.
//! 2. Reach it over HTTPS. Failure only at this stage means the name is fine
//!    and the certificate is not trusted yet, which almost always means step 2
//!    of the install was missed.
//!
//! A verification step that distinguishes those two is worth more than any
//! quantity of instructions, because it converts a silent failure into a
//! diagnosis.

use std::net::IpAddr;

/// Content type that makes iOS treat the download as a profile to install
/// rather than a file to save. Serving it as `text/plain` produces a page of
/// base64 and no install prompt.
pub const CA_CONTENT_TYPE: &str = "application/x-x509-ca-cert";

/// The public half of the TLS setup, as the HTTP surface needs to see it.
///
/// Deliberately contains no key material of any kind. The type is the guard:
/// the install page is rendered from this and only this, so there is no path by
/// which a private key could reach a response, and a future edit that tried
/// would have to add a field here first.
#[derive(Clone)]
pub struct TlsPublicInfo {
    /// PEM of the CA's public certificate.
    pub ca_cert_pem: String,
    pub ca_display_name: String,
    /// The mDNS name the certificate is issued for.
    pub hostname: String,
    pub http_port: u16,
    pub https_port: u16,
    /// LAN address, for networks where multicast does not survive.
    pub ip: Option<IpAddr>,
    /// Random per-process value, echoed by `/trustcheck`.
    ///
    /// Without it the trust check cannot tell *this* bridge from any other host
    /// answering `coyote.local:8443` with a certificate the phone trusts. Two
    /// bridges on one machine both register the name, so B's install page can
    /// probe A's listener, report "Trusted", and send the phone to A carrying
    /// B's token — which arrives as a 401 and gets debugged as an auth bug.
    pub instance_nonce: String,
}

/// What the install page needs to render itself and its links.
pub struct InstallPage<'a> {
    /// The mDNS name, when it is being advertised.
    pub hostname: &'a str,
    /// The LAN address, as a fallback for networks that block multicast.
    pub ip: Option<IpAddr>,
    pub http_port: u16,
    pub https_port: u16,
    /// How the certificate will appear in iOS's Settings list.
    pub ca_display_name: &'a str,
    /// Echoed by `/trustcheck`, so the page can confirm it reached *this*
    /// bridge rather than another one answering the same name.
    pub instance_nonce: &'a str,
    /// The query string from the pairing URL, `?t=…` included, or empty.
    ///
    /// Carried through verbatim rather than reconstructed: the pairing token is
    /// owned elsewhere, and a page that rebuilt the query would silently drop
    /// any parameter added later. Silently is the operative word — the phone
    /// would load the app and then fail to authorize, which looks like a TLS
    /// fault and is not one.
    pub query: &'a str,
}

impl TlsPublicInfo {
    /// Build the page description, carrying `query` (the pairing token, when
    /// there is one) through untouched.
    pub fn install_page<'a>(&'a self, query: &'a str) -> InstallPage<'a> {
        InstallPage {
            hostname: &self.hostname,
            ip: self.ip,
            http_port: self.http_port,
            https_port: self.https_port,
            ca_display_name: &self.ca_display_name,
            instance_nonce: &self.instance_nonce,
            query,
        }
    }
}

impl<'a> InstallPage<'a> {
    /// Where the phone should end up once the certificate is trusted.
    pub fn https_url(&self) -> String {
        format!(
            "https://{}:{}/{}",
            self.hostname, self.https_port, self.query
        )
    }

    /// Stage one probes the *same* endpoint over plain HTTP rather than
    /// something like `/healthz`. Purpose-built and ungated, so the reachability
    /// answer never depends on how another route's authorization is configured —
    /// a 401 and a working network would otherwise be indistinguishable from
    /// each other in a `fetch` result.
    fn http_probe_url(&self) -> String {
        format!("http://{}:{}/trustcheck", self.hostname, self.http_port)
    }

    fn https_probe_url(&self) -> String {
        format!("https://{}:{}/trustcheck", self.hostname, self.https_port)
    }

    /// The Bluetooth check, on the HTTPS origin, **without the token**.
    ///
    /// The query is dropped deliberately. `/secure-check` ignores a token, and
    /// this is the one link on the page the user is told to open in *Bluefy* —
    /// so carrying it would copy a password-equivalent string into a second
    /// browser's history and leave it there, buying nothing.
    pub fn secure_check_url(&self) -> String {
        to_https_origin(
            &format!("http://{}:{}/secure-check", self.hostname, self.http_port),
            self.hostname,
            self.https_port,
        )
    }

    fn ip_fallback_url(&self) -> Option<String> {
        self.ip
            .map(|ip| format!("https://{}:{}/{}", ip, self.https_port, self.query))
    }
}

/// The path the QR must point at, given whether a certificate exists.
///
/// A function rather than an inline `match` at each call site because there are
/// three of them — the headless binary, the app, and the log line — and when
/// they were written separately they disagreed. The QR kept the bare root while
/// the log said `/install`, so scanning it landed the phone in the app over
/// plain HTTP: no secure context, no Web Bluetooth, and the certificate never
/// offered. It looks exactly like the app being broken.
pub fn pairing_base(http_origin: &str, tls_available: bool) -> String {
    if tls_available {
        format!("{http_origin}/install")
    } else {
        http_origin.to_string()
    }
}

/// Rewrite a pairing URL onto the HTTPS origin, **preserving the query string**.
///
/// The query carries the pairing token. Swapping scheme and authority while
/// dropping everything after `?` is the one way this module can silently break
/// authorization, so it is a named function with a test rather than an inline
/// `format!`.
pub fn to_https_origin(url: &str, hostname: &str, https_port: u16) -> String {
    let rest = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url);
    // Everything from the first `/` after the authority — path, query and
    // fragment together, untouched.
    let tail = match rest.find('/') {
        Some(i) => &rest[i..],
        None => "/",
    };
    format!("https://{hostname}:{https_port}{tail}")
}

/// Split a request path into its path and its query (`?` included, or empty).
pub fn split_query(path: &str) -> (&str, &str) {
    match path.find('?') {
        Some(i) => (&path[..i], &path[i..]),
        None => (path, ""),
    }
}

/// The body served over TLS to prove the certificate is trusted.
///
/// Deliberately tiny and free of anything worth reading: reaching it at all is
/// the entire signal. It carries its own permissive CORS header because the
/// page asking the question is on a different origin (plain HTTP, different
/// port) by necessity — that is what makes it a *cross-origin* check, and a
/// check that could only be run from the same origin would answer nothing.
pub const TRUSTCHECK_BODY: &str = "trusted";

/// A random per-process value for [`TlsPublicInfo::instance_nonce`].
pub fn new_instance_nonce() -> String {
    let mut bytes = [0u8; 8];
    // Failure here is not worth aborting a bridge over; a fixed value only
    // costs the ability to tell two instances apart, which is what the nonce
    // buys and not something anything depends on for safety.
    let _ = getrandom::getrandom(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn page(cfg: &InstallPage<'_>) -> String {
    let https_url = html_escape(&cfg.https_url());
    let secure_check_url = html_escape(&cfg.secure_check_url());
    let http_probe = html_escape(&cfg.http_probe_url());
    let https_probe = html_escape(&cfg.https_probe_url());
    let ca_name = html_escape(cfg.ca_display_name);
    let nonce = html_escape(cfg.instance_nonce);
    let host = html_escape(cfg.hostname);
    let fallback = match cfg.ip_fallback_url() {
        Some(url) => format!(
            r#"<p class="hint">If the name never resolves, this network is probably blocking
multicast. You can use <a href="{u}">{u}</a> instead — it works until this
computer's address changes.</p>"#,
            u = html_escape(&url)
        ),
        None => String::new(),
    };

    format!(
        r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Set up a secure connection</title>
<style>
:root {{ color-scheme: light dark; }}
body {{ font: 16px/1.6 system-ui, sans-serif; margin: 0 auto; max-width: 34rem; padding: 1.5rem; }}
h1 {{ font-size: 1.4rem; line-height: 1.3; }}
h2 {{ font-size: 1.05rem; margin-top: 2rem; }}
ol {{ padding-left: 1.3rem; }}
li {{ margin: .6rem 0; }}
code, kbd {{ background: color-mix(in srgb, currentColor 12%, transparent);
  padding: .1rem .35rem; border-radius: .25rem; font-size: .95em; }}
kbd {{ white-space: nowrap; }}
.btn {{ display: inline-block; font: inherit; font-weight: 600; text-decoration: none;
  text-align: center; padding: .8rem 1.2rem; border-radius: .6rem; border: 1px solid currentColor;
  background: transparent; color: inherit; cursor: pointer; }}
.btn.primary {{ background: color-mix(in srgb, currentColor 14%, transparent); }}
.row {{ display: flex; gap: .75rem; flex-wrap: wrap; margin: 1rem 0; }}
#result {{ margin: 1rem 0; padding: .9rem 1rem; border-radius: .6rem; display: none;
  border: 1px solid color-mix(in srgb, currentColor 30%, transparent); }}
#result.show {{ display: block; }}
#result strong {{ display: block; margin-bottom: .25rem; }}
.hint {{ opacity: .75; font-size: .92rem; }}
.trade {{ border-left: 3px solid color-mix(in srgb, currentColor 35%, transparent);
  padding-left: .9rem; margin: 1.5rem 0; }}
#next {{ display: none; }}
#next.show {{ display: block; }}
.hidden {{ display: none; }}
#ready h1 {{ margin-bottom: .25rem; }}
</style></head><body>

<!--
  Two states, one page, decided by the bridge rather than by the user.

  There used to be a separate QR for the app and a user who had to know which
  one they wanted — and the app QR skipped the certificate entirely, so scanning
  the obvious one produced an app that could never find the Coyote. `/trustcheck`
  means we can just ask: the phone already knows whether it trusts us, and one
  fetch turns that into the right page.

  The check has to run on the phone, not on the desktop that rendered the QR,
  which is why this is client-side rather than decided server-side.
-->
<div id="ready" class="hidden">
  <h1>You are set up</h1>
  <p>This phone already trusts this bridge, so there is nothing to install.</p>
  <div class="row">
    <a class="btn primary" href="{https_url}">Open the app</a>
    <a class="btn" href="{secure_check_url}">Test Bluetooth</a>
  </div>
  <p class="hint">Open the app in <strong>Bluefy</strong>. Safari does not support Web
  Bluetooth, so there it will load and never find the Coyote.</p>
  <!-- Single-quoted href on purpose: a double quote followed by a hash would
       close the Rust raw-string literal this page lives in. -->
  <p class="hint"><a href='#' id="show-setup">Show the certificate instructions anyway</a></p>
</div>

<div id="setup" class="hidden">

<h1>Set up a secure connection to this bridge</h1>

<p>Your phone needs a secure connection before it can talk to the Coyote over
Bluetooth &mdash; that is a browser rule, not a choice this app makes. Installing
the certificate below is what provides one, and you only do it once.</p>

<div class="row">
  <a class="btn primary" href="/ca.crt">Download the certificate</a>
</div>
<p class="hint">On an iPhone this must be done in <strong>Safari</strong>. Other browsers
download the file without offering to install it.</p>

<div class="trade">
<p><strong>You will need two browsers, and that is expected.</strong> Install the
certificate here in <strong>Safari</strong> — it is the only iOS browser that offers to
install one. Then open the app in <strong>Bluefy</strong>, because
<strong>Safari does not support Web Bluetooth at all</strong> and never has.</p>
<p class="hint">If you open the app in Safari it will load and simply not find the
Coyote. That is not a certificate problem and re-installing will not fix it.
The certificate you install here is trusted system-wide, so Bluefy gets it too.</p>
</div>

<h2>On iPhone or iPad, in this order</h2>
<ol>
  <li>Tap <strong>Download the certificate</strong> above, then <strong>Allow</strong>.</li>
  <li>Open <kbd>Settings</kbd>. Near the top you will see
      <strong>Profile Downloaded</strong> &mdash; tap it, then <strong>Install</strong>
      (top right), enter your passcode, and <strong>Install</strong> again.</li>
  <li><strong>This is the step that gets missed.</strong> Go to
      <kbd>Settings &rsaquo; General &rsaquo; About &rsaquo; Certificate&nbsp;Trust&nbsp;Settings</kbd>
      and turn <strong>on</strong> the switch next to
      &ldquo;{ca_name}&rdquo;.</li>
</ol>
<p class="hint">Until step 3 is done the certificate is installed but not trusted, and
the connection fails in exactly the same way as if you had never installed it.
That is why the button below exists.</p>

<p class="hint"><strong>And it will not fail the way you expect.</strong> Browsing to
an untrusted address shows a warning page you can read. The app does not browse —
it opens a socket, and an untrusted socket is refused <em>silently</em>, with no
warning and no mention of certificates. It surfaces as
&ldquo;bridge unreachable&rdquo;, so the natural reaction is to go and check the
Wi‑Fi. Same certificate, same address, two completely different-looking failures.
If this page works and the app still cannot connect, suspect trust, not the
network.</p>

<h2>Did it work?</h2>
<div class="row">
  <button class="btn primary" id="check">Check my trust</button>
</div>
<div id="result" role="status" aria-live="polite"></div>
<div id="next" class="row">
  <a class="btn primary" id="go" href="{https_url}">Open the app</a>
  <a class="btn" href="{secure_check_url}">Test Bluetooth</a>
</div>
{fallback}
</div>

<h2>What you are agreeing to</h2>
<div class="trade">
<p>A trusted certificate authority can vouch for <em>any</em> website to this
device, not only this one. That is a real thing to hand out, so here is what
limits it:</p>
<ul>
  <li>This authority was generated <strong>on your own computer</strong>, the first
      time the bridge ran. Nothing was downloaded, and no copy of it exists
      anywhere else.</li>
  <li>Its private key &mdash; the part that does the vouching &mdash; never leaves that
      computer. This page only ever sends you the public certificate.</li>
  <li>It is named <strong>&ldquo;{ca_name}&rdquo;</strong> so you can find it later.</li>
</ul>
<p><strong>To remove it:</strong> <kbd>Settings &rsaquo; General &rsaquo; VPN &amp; Device
Management</kbd>, select the profile, then <strong>Remove Profile</strong>. Deleting
the <code>tls</code> folder in the bridge's settings directory removes the
authority from the computer as well.</p>
</div>

<script>
(function () {{
  var button = document.getElementById('check');
  var result = document.getElementById('result');
  var next = document.getElementById('next');

  function say(title, detail) {{
    result.innerHTML = '';
    var strong = document.createElement('strong');
    strong.textContent = title;
    result.appendChild(strong);
    result.appendChild(document.createTextNode(detail));
    result.className = 'show';
  }}

  // Cache-busting matters more than usual here: a cached success would tell
  // someone their certificate is trusted after they had removed it.
  function probe(url) {{
    return fetch(url + (url.indexOf('?') < 0 ? '?' : '&') + 'cb=' + Date.now(),
                 {{ cache: 'no-store', mode: 'cors' }});
  }}

  var ready = document.getElementById('ready');
  var setup = document.getElementById('setup');
  var settled = false;

  // Decide which of the two pages this is. Until the check answers, show
  // neither: a flash of "install this certificate" for someone who already did
  // is the exact confusion this page exists to remove.
  function settle(trusted) {{
    if (settled) return;
    settled = true;
    (trusted ? ready : setup).className = '';
  }}

  document.getElementById('show-setup').addEventListener('click', function (e) {{
    e.preventDefault();
    setup.className = '';
  }});

  function runCheck(manual) {{
    if (manual) {{
      button.disabled = true;
      say('Checking…', '');
    }}

    // Stage one: can we reach the bridge by name at all? A failure here is a
    // name or network problem and has nothing to do with the certificate, so
    // it must not be reported as one.
    probe('{http_probe}').then(function () {{
      // Stage two: the same machine over TLS. Reaching it means the phone
      // validated our certificate, which means the root is installed AND
      // trusted. Nothing else produces this result.
      return probe('{https_probe}').then(function (r) {{
        return r.text();
      }}).then(function (body) {{
        // Confirm it is *this* bridge. Two instances both register
        // `coyote.local`, so without the nonce this page could cheerfully
        // certify a different bridge's listener and then send the phone there
        // carrying a token that bridge has never heard of.
        if (body.indexOf('{nonce}') < 0) {{
          say('Reached a different bridge.',
              'Something else on this network is answering “{host}” over HTTPS. ' +
              'The certificate is fine, but the phone would be sent to the wrong ' +
              'bridge. Stop the other instance, or use the numeric address below.');
          return;
        }}
        say('Trusted — you are all set.',
            'Your phone accepts this bridge’s certificate. You can open the app now.');
        next.className = 'row show';
        settle(true);
      }}).catch(function () {{
        say('Not trusted yet.',
            'The bridge is reachable, so the network and the address are fine — ' +
            'it is the certificate. This almost always means step 3 above was missed: ' +
            'Settings › General › About › Certificate Trust Settings, ' +
            'and switch on “{ca_name}”. Then check again.');
      }});
    }}).catch(function () {{
      say('Cannot reach the bridge at “{host}”.',
          'This is not a certificate problem — the name did not resolve, or this ' +
          'network blocks devices from seeing each other. Check the phone is on the same ' +
          'Wi‑Fi as the computer, and not on a guest network.');
    }}).then(function () {{
      button.disabled = false;
      // Whatever happened, this is no longer undecided: anything that did not
      // reach the "trusted" branch needs the instructions.
      settle(false);
    }});
  }}

  button.addEventListener('click', function () {{ runCheck(true); }});

  // Run once on load, so the QR has one destination and the bridge works out
  // which half of it the phone needs.
  runCheck(false);
}})();
</script>
</body></html>"#
    )
}

/// The page that answers the actual acceptance question.
///
/// Everything else here establishes that the certificate is well-formed and
/// trusted. That is necessary and it is not the point. The point is whether the
/// **browser** will hand this origin a Bluetooth device, and only the browser
/// can answer that.
///
/// Served on the TLS listener, because that is the origin under test — asking
/// this question over plain HTTP would always answer "no" and prove nothing.
/// It reports three things separately, because they fail for different reasons:
///
/// 1. `isSecureContext` — whether the browser considers this origin secure at
///    all. False here means the certificate is not trusted, whatever the page
///    looks like.
/// 2. `navigator.bluetooth` — whether the browser implements Web Bluetooth.
///    On iOS, Safari does **not**; only Bluefy and a few other WebKit-shell
///    browsers do. A user who has done everything right and is in Safari will
///    fail here, and that is worth saying plainly rather than letting them
///    re-check a certificate that was never the problem.
/// 3. An actual `requestDevice` call, which is the only thing that proves the
///    chooser opens.
pub fn secure_check_page() -> String {
    r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Can this page use Bluetooth?</title>
<style>
:root { color-scheme: light dark; }
body { font: 16px/1.6 system-ui, sans-serif; margin: 0 auto; max-width: 32rem; padding: 1.5rem; }
h1 { font-size: 1.3rem; }
.row { display: flex; gap: .6rem; align-items: baseline; margin: .5rem 0; }
.k { font-weight: 600; min-width: 11rem; }
.btn { font: inherit; font-weight: 600; padding: .8rem 1.2rem; border-radius: .6rem;
  border: 1px solid currentColor; background: transparent; color: inherit; cursor: pointer; margin: 1rem 0; }
#out { padding: .9rem 1rem; border-radius: .6rem; display: none;
  border: 1px solid color-mix(in srgb, currentColor 30%, transparent); }
#out.show { display: block; }
.hint { opacity: .75; font-size: .92rem; }
code { background: color-mix(in srgb, currentColor 12%, transparent); padding: .1rem .35rem; border-radius: .25rem; }
</style></head><body>
<h1>Can this page use Bluetooth?</h1>
<p class="hint">This is the question the certificate exists to make answerable.
Everything else only sets it up.</p>

<p class="hint"><strong>Why this page matters more than it looks.</strong> If the app
reports the bridge as unreachable, that is <em>not</em> evidence about the network.
An untrusted certificate makes the app's socket fail silently — no warning, no
mention of certificates, just a dead connection. This page is the only thing that
can tell the two apart: <strong>if this page loads at all, the certificate is
trusted and the network is fine.</strong></p>

<div class="row"><span class="k">Origin</span><code id="origin"></code></div>
<div class="row"><span class="k">Secure context</span><span id="secure"></span></div>
<div class="row"><span class="k">Web Bluetooth API</span><span id="api"></span></div>

<button class="btn" id="go">Ask for a Bluetooth device</button>
<div id="out" role="status" aria-live="polite"></div>

<p class="hint" id="safari" style="display:none">
<strong>Expected, if you are in Safari.</strong> Safari on iOS does not implement Web
Bluetooth — that is a known, settled limitation and no certificate changes it.
Open this same URL in <strong>Bluefy</strong>, which does.</p>

<p class="hint" id="untrusted" style="display:none">
<strong>Worth reporting.</strong> Secure context is false here. If Safari reported
&ldquo;Trusted&rdquo; for the same address, then this browser is not honouring the
certificate you installed in iOS Settings — which is a different problem from a bad
certificate, and re-installing will not fix it.</p>

<script>
(function () {
  var secure = window.isSecureContext;
  var api = typeof navigator.bluetooth !== 'undefined';
  document.getElementById('origin').textContent = location.origin;
  document.getElementById('secure').textContent = secure ? 'yes' : 'NO — the certificate is not trusted here';
  document.getElementById('api').textContent = api ? 'available' : 'MISSING in this browser';
  // Two different findings, and they must not be confused. No API in a secure
  // context is the known Safari limitation. A browser that has the API but does
  // not consider this origin secure is the open question — whether a
  // third-party WebKit shell honours a CA installed in the iOS system store.
  if (secure && !api) { document.getElementById('safari').style.display = 'block'; }
  if (!secure && api) { document.getElementById('untrusted').style.display = 'block'; }

  var out = document.getElementById('out');
  function say(t) { out.textContent = t; out.className = 'show'; }

  document.getElementById('go').addEventListener('click', function () {
    if (!secure) { say('Not a secure context, so the browser will not offer Bluetooth. Go back and finish installing the certificate.'); return; }
    if (!api) { say('This browser does not implement Web Bluetooth. See the note below.'); return; }
    // acceptAllDevices, because this is a capability probe rather than a real
    // connection — filtering to the Coyote would fail for a user who simply
    // does not have one switched on, which is a different answer.
    navigator.bluetooth.requestDevice({ acceptAllDevices: true })
      .then(function (d) { say('Bluetooth works. Picked: ' + (d.name || '(unnamed device)')); })
      .catch(function (e) {
        if (e && e.name === 'NotFoundError') {
          say('The chooser opened and you dismissed it (or nothing was nearby). That still proves Bluetooth works from this origin.');
        } else {
          say('Failed: ' + (e && e.name ? e.name : '') + ' ' + (e && e.message ? e.message : e));
        }
      });
  });
})();
</script>
</body></html>"#
        .to_string()
}

/// Escape for HTML text and double-quoted attributes.
///
/// Everything interpolated into the page is locally generated, but the machine
/// name reaches the CA display name from the environment, so it is not a
/// constant and is not treated as one.
fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ctx<'a>(query: &'a str) -> InstallPage<'a> {
        InstallPage {
            hostname: "coyote.local",
            ip: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 9))),
            http_port: 8787,
            https_port: 8443,
            ca_display_name: "CoyoteSocket Bridge on JUSTIN-G",
            instance_nonce: "abc123nonce",
            query,
        }
    }

    #[test]
    fn one_page_serves_both_states_and_starts_showing_neither() {
        // The user asked why there were two QRs and which one to scan. There is
        // now one, and the phone's own answer to `/trustcheck` decides which
        // half it sees — the bridge can ask rather than making the user choose.
        let page = page(&ctx(""));
        assert!(page.contains(r#"id="ready""#));
        assert!(page.contains(r#"id="setup""#));
        // Both start hidden: a flash of "install this certificate" shown to
        // someone who already installed it is the confusion this removes.
        assert!(page.contains(r#"<div id="ready" class="hidden">"#));
        assert!(page.contains(r#"<div id="setup" class="hidden">"#));
        // And the check runs itself rather than waiting to be pressed.
        assert!(page.contains("runCheck(false);"));
    }

    #[test]
    fn the_qr_points_at_the_install_page_whenever_a_certificate_exists() {
        // Regression. The QR used to encode the bare root while the log said
        // `/install`, so scanning it — the path every instruction leads with —
        // dropped the phone into the app over plain HTTP, where Web Bluetooth
        // cannot work and nothing ever offers the certificate.
        assert_eq!(
            pairing_base("http://192.168.0.9:8788", true),
            "http://192.168.0.9:8788/install"
        );
        // And without a certificate there is nothing to install, so the root is
        // right.
        assert_eq!(
            pairing_base("http://192.168.0.9:8788", false),
            "http://192.168.0.9:8788"
        );
    }

    #[test]
    fn the_bluetooth_check_link_does_not_leak_the_token_into_a_second_browser() {
        // The page tells the user to open this in Bluefy. `/secure-check`
        // ignores a token, so appending one only writes a password-equivalent
        // string into a second browser's history and leaves it there.
        let page = page(&ctx("?t=secret-token"));
        assert!(page.contains("https://coyote.local:8443/secure-check"));
        assert!(
            !page.contains("secure-check?t="),
            "the Bluetooth check must not carry the pairing token"
        );
    }

    #[test]
    fn the_pairing_token_survives_the_move_to_https() {
        // The single most breakable contract between this work and the pairing
        // token: a QR that loads a page which then fails to authorize looks
        // like a TLS fault and is not one.
        assert_eq!(
            to_https_origin("http://192.168.0.9:8787/install?t=abc123", "coyote.local", 8443),
            "https://coyote.local:8443/install?t=abc123"
        );
    }

    #[test]
    fn every_query_parameter_survives_not_just_the_first() {
        assert_eq!(
            to_https_origin("http://h:1/?t=a&next=%2Fsettings", "coyote.local", 8443),
            "https://coyote.local:8443/?t=a&next=%2Fsettings"
        );
    }

    #[test]
    fn a_bare_authority_still_produces_a_valid_url() {
        assert_eq!(
            to_https_origin("http://192.168.0.9:8787", "coyote.local", 8443),
            "https://coyote.local:8443/"
        );
    }

    #[test]
    fn the_query_is_split_off_without_being_altered() {
        assert_eq!(split_query("/install?t=abc"), ("/install", "?t=abc"));
        assert_eq!(split_query("/install"), ("/install", ""));
        assert_eq!(split_query("/?t=a&b=c"), ("/", "?t=a&b=c"));
    }

    #[test]
    fn the_continue_link_carries_the_token_through_to_the_app() {
        let page = page(&ctx("?t=secret-token"));
        assert!(
            page.contains("https://coyote.local:8443/?t=secret-token"),
            "the app link must carry the pairing token or the phone cannot authorize"
        );
    }

    #[test]
    fn the_page_names_the_trust_step_that_is_otherwise_missed() {
        let page = page(&ctx(""));
        assert!(page.contains("Certificate&nbsp;Trust&nbsp;Settings"));
        assert!(page.contains("Profile Downloaded"));
        // Removal has to be documented, not just installation.
        assert!(page.contains("Remove Profile"));
    }

    #[test]
    fn the_check_distinguishes_an_unreachable_name_from_an_untrusted_cert() {
        let page = page(&ctx(""));

        // The two stages differ only in scheme and port, and that is the whole
        // trick: same host, same path, so the only variable between them is
        // whether TLS validated. Anything else would confound the two answers.
        assert!(
            page.contains("http://coyote.local:8787/trustcheck"),
            "stage one must probe plain HTTP, to establish the name resolves"
        );
        assert!(
            page.contains("https://coyote.local:8443/trustcheck"),
            "stage two must probe TLS, to establish the certificate is trusted"
        );

        // And the two failures must read differently, because they need
        // different actions from the user.
        assert!(page.contains("Not trusted yet."));
        assert!(page.contains("Cannot reach the bridge"));
    }

    #[test]
    fn the_page_states_the_trade_rather_than_burying_it() {
        let page = page(&ctx(""));
        assert!(page.contains("any</em> website"));
        assert!(page.contains("never leaves that"));
    }

    #[test]
    fn the_bluetooth_check_reports_the_three_things_that_fail_separately() {
        let page = secure_check_page();
        // Secure context: false here means the certificate is not trusted,
        // whatever else the page says.
        assert!(page.contains("isSecureContext"));
        // API presence: Safari on iOS does not implement Web Bluetooth at all,
        // and a user who has done everything right will otherwise blame the
        // certificate for a browser limitation.
        assert!(page.contains("navigator.bluetooth"));
        assert!(page.contains("Bluefy"));
        // And the only thing that actually proves it: the chooser opening.
        assert!(page.contains("requestDevice"));
    }

    #[test]
    fn the_install_page_links_to_the_bluetooth_check_over_https() {
        // Over plain HTTP the check would always answer "not secure" — true,
        // and useless. It has to be reached on the origin under test.
        let page = page(&ctx(""));
        assert!(page.contains("https://coyote.local:8443/secure-check"));
    }

    #[test]
    fn the_bluetooth_check_link_goes_through_the_tested_rewrite() {
        // `to_https_origin` had tests and no callers, so a reviewer sent to
        // check it was reviewing code nothing ran. It is now the only thing
        // that builds this URL.
        let page = page(&ctx("?t=tok"));
        assert!(page.contains("https://coyote.local:8443/secure-check"));
    }

    #[test]
    fn the_trust_check_confirms_it_reached_this_bridge_and_not_another() {
        // Two bridges both register `coyote.local`. Without the nonce, one
        // instance's install page can certify the other's listener and send the
        // phone there with a token that bridge has never seen — arriving as a
        // 401 and debugged as an auth bug.
        let page = page(&ctx(""));
        assert!(page.contains("abc123nonce"));
        assert!(page.contains("Reached a different bridge."));
    }

    #[test]
    fn both_pages_say_a_failing_socket_is_not_evidence_about_the_network() {
        // An untrusted certificate makes the app's WebSocket fail silently —
        // no interstitial, no mention of certificates — so it presents as
        // "bridge unreachable" and sends the user to check their Wi-Fi. These
        // two pages are the only things that can tell trust from network.
        let install = page(&ctx(""));
        assert!(install.contains("suspect trust, not the"));
        let check = secure_check_page();
        assert!(check.contains("if this page loads at all, the certificate is"));
    }

    #[test]
    fn the_certificate_link_does_not_carry_a_download_attribute() {
        // iOS Safari honours `download` by saving to Files, which is the
        // opposite of the profile-install prompt the content type is chosen to
        // trigger. mkcert and Caddy both link plainly for the same reason.
        let page = page(&ctx(""));
        assert!(page.contains(r#"href="/ca.crt""#));
        assert!(
            !page.contains("download="),
            "a download attribute defeats the iOS profile-install prompt"
        );
    }

    #[test]
    fn a_hostile_machine_name_cannot_inject_markup() {
        let page = page(&InstallPage {
            ca_display_name: "<script>alert(1)</script>",
            ..ctx("")
        });
        assert!(!page.contains("<script>alert(1)</script>"));
        assert!(page.contains("&lt;script&gt;"));
    }
}
