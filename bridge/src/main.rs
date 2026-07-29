//! Bridge entry point.
//!
//! The tokio runtime runs on a background thread and the tray owns the main
//! thread, because on Windows and macOS the tray's event loop must be on the
//! main thread and never returns. With `--no-tray` (or a build without the
//! `tray` feature) the main thread just parks instead.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use coyote_bridge::auth;
use coyote_bridge::wire::Tap;
use coyote_bridge::{http, logging, mdns, supervisor, tls};
use coyote_bridge::{log_info, log_warn};
use tokio::net::TcpListener;

const DEFAULT_PLAYER_PORT: u16 = 23554;
const DEFAULT_HTTP_PORT: u16 = 8787;
/// The conventional "alternate HTTPS" port. Above 1024, so no privileges are
/// needed to bind it on any platform.
const DEFAULT_HTTPS_PORT: u16 = 8443;

struct Args {
    player: String,
    bind: IpAddr,
    http_port: u16,
    static_dir: Option<PathBuf>,
    /// Directory of funscripts to serve at `/library`. `None` means no
    /// library, which is a normal state rather than an error.
    library_dir: Option<PathBuf>,
    log_dir: Option<PathBuf>,
    tray: bool,
    /// Pin the pairing token instead of minting a fresh one each start. For a
    /// service under a supervisor, where a URL that changes on every restart is
    /// worse than a secret in a unit file.
    token: Option<auth::Token>,
    /// Port for the TLS listener — the one Web Bluetooth needs.
    https_port: u16,
    /// Where the local CA is kept between runs. Losing it costs every phone a
    /// reinstall, so for a service it should point somewhere durable rather
    /// than at a working directory.
    tls_dir: Option<PathBuf>,
    /// Serve plain HTTP only. The phone will not be able to reach the Coyote.
    tls: bool,
    /// Address to put in the certificate, the mDNS record and the QR.
    ///
    /// Detection follows the default route, which is right on an ordinary LAN
    /// and wrong behind a full-tunnel VPN — it would report the tunnel address,
    /// which the phone cannot reach. Same failure shape as the virtual-adapter
    /// problem that made the bridge run its own mDNS responder.
    advertise: Option<IpAddr>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            // Loopback by default: useful when the player is on this machine,
            // and harmless when it is not. The Quest's IP has to be typed in,
            // exactly as MultiFunPlayer requires.
            player: format!("127.0.0.1:{DEFAULT_PLAYER_PORT}"),
            // The phone has to reach us, so listening only on loopback would
            // defeat the point. Same reasoning as the desktop app's T-Code
            // server binding 0.0.0.0.
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            http_port: DEFAULT_HTTP_PORT,
            static_dir: None,
            library_dir: None,
            log_dir: None,
            tray: true,
            token: None,
            https_port: DEFAULT_HTTPS_PORT,
            tls_dir: None,
            // On by default. A bridge without a secure context cannot do the
            // one thing the phone is there for, so opting out has to be the
            // deliberate choice rather than the accidental one.
            tls: true,
            advertise: None,
        }
    }
}

const USAGE: &str = "\
coyote-bridge (spike) - DeoVR/HereSphere to phone bridge

USAGE:
  coyote-bridge [OPTIONS]

OPTIONS:
  --player <host[:port]>  Player's remote-control endpoint.
                          Port defaults to 23554. [default: 127.0.0.1:23554]
  --bind <ip>             Address to serve on. [default: 0.0.0.0]
  --http-port <port>      Port to serve on. [default: 8787]
  --static-dir <path>     Directory to serve - the PWA's `dist`.
  --library-dir <path>    Directory of .funscript files to serve at /library.
                          Default: none, which is a normal state - the phone
                          simply sees an empty library.
  --log-dir <path>        Where to write coyote-bridge.log. [default: cwd]
  --no-tray               Do not create a tray icon (headless servers).
  --token <hex>           Pairing token. Default: a fresh one each start, which
                          means the phone URL changes on every restart.
  --https-port <port>     Port for the TLS listener. [default: 8443]
  --tls-dir <path>        Where to keep the local CA between runs.
                          [default: %APPDATA%/com.coyotesocket.bridge, or
                          $XDG_CONFIG_HOME/$HOME equivalent]
  --advertise <ip>        Address to put in the certificate, the mDNS record
                          and the QR. Default: the interface of the default
                          route. Set this if a VPN or a second NIC makes the
                          bridge advertise an address the phone cannot reach.
  --no-tls                Serve plain HTTP only. The phone will NOT be able to
                          use Bluetooth: browsers require a secure context.
  -h, --help              Show this.

NOTE: DeoVR and HereSphere do not listen on 23554 until remote control is
      enabled in the player's own settings. If the bridge reports the player
      unreachable, check that first.

NOTE: On first run the bridge generates a certificate authority and keeps its
      private key in --tls-dir. The key never leaves this machine. Open the
      pairing URL on the phone and follow the instructions to install the
      public certificate; without it the phone cannot use Bluetooth.
      Deleting --tls-dir means every phone has to install a new one.
";

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);

    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} expects a value"));
        match flag.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--player" => {
                let raw = value()?;
                // Accept a bare host and fill in the well-known port; typing
                // the IP is already the fiddly part.
                args.player = if raw.contains(':') {
                    raw
                } else {
                    format!("{raw}:{DEFAULT_PLAYER_PORT}")
                };
            }
            "--bind" => {
                args.bind = value()?
                    .parse()
                    .map_err(|e| format!("--bind is not an IP address: {e}"))?
            }
            "--http-port" => {
                args.http_port = value()?
                    .parse()
                    .map_err(|e| format!("--http-port is not a port: {e}"))?
            }
            "--static-dir" => args.static_dir = Some(PathBuf::from(value()?)),
            "--library-dir" => args.library_dir = Some(PathBuf::from(value()?)),
            "--log-dir" => args.log_dir = Some(PathBuf::from(value()?)),
            "--no-tray" => args.tray = false,
            "--token" => args.token = Some(coyote_bridge::auth::Token::from_string(value()?)),
            "--https-port" => {
                args.https_port = value()?
                    .parse()
                    .map_err(|e| format!("--https-port is not a port: {e}"))?
            }
            "--tls-dir" => args.tls_dir = Some(PathBuf::from(value()?)),
            "--advertise" => {
                args.advertise = Some(
                    value()?
                        .parse()
                        .map_err(|e| format!("--advertise is not an IP address: {e}"))?,
                )
            }
            "--no-tls" => args.tls = false,
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    logging::init(args.log_dir.clone());

    if let Some(dir) = &args.static_dir {
        if !dir.is_dir() {
            log_warn!(
                "[main] --static-dir {} does not exist; serving the placeholder page instead",
                dir.display()
            );
        }
    }

    // Resolved once and threaded everywhere: the certificate's IP SAN, the
    // mDNS record, the QR and the allowed origins must all name the same
    // interface. Three independent calls to `local_ip()` only usually agreed,
    // and disagreed exactly when detection failed.
    let advertised_ip = args
        .advertise
        .or_else(http::local_ip)
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let base_url = format!("http://{advertised_ip}:{}", args.http_port);
    let local_url = format!("http://127.0.0.1:{}", args.http_port);

    // The headless binary has no settings file, so its token is per-run. That
    // is a real behaviour change: the URL to hand a phone now differs on every
    // start. It is the honest default for a service with nowhere to persist a
    // secret, and the alternative — no token at all — leaves any web page the
    // user visits able to drive their player. The app build persists its token
    // (see `bridge-app/src/settings.rs`); a long-lived deployment should
    // eventually do the same.
    let token = args.token.clone().unwrap_or_else(auth::Token::generate);

    // Set up the certificate before anything binds, because the answer changes
    // what the QR should say. Failure is not fatal: plain HTTP still serves the
    // app, and losing the secure context should cost Web Bluetooth and nothing
    // else.
    // Never the working directory. That default wrote a CA private key into
    // whatever folder the binary was launched from — for a developer, a git
    // checkout, one `git add -A` from publishing a key that can impersonate any
    // site to every phone that trusted it.
    let tls_config_dir = args
        .tls_dir
        .clone()
        .unwrap_or_else(coyote_bridge::certs::default_config_dir);
    let prepared = if args.tls {
        match tls::prepare(
            &tls_config_dir,
            args.http_port,
            args.https_port,
            Some(advertised_ip),
        ) {
            Ok(prepared) => Some(prepared),
            Err(e) => {
                log_warn!(
                    "[main] could not set up TLS ({e}); serving plain HTTP only. \
                     The phone will not be able to use Bluetooth until this is fixed."
                );
                None
            }
        }
    } else {
        log_warn!(
            "[main] --no-tls: serving plain HTTP only. Browsers require a secure context \
             for Bluetooth, so the phone will not be able to reach the Coyote."
        );
        None
    };

    // The QR points at the **install page on plain HTTP**, not at HTTPS.
    // A phone that has not yet trusted the CA meets a full-page certificate
    // interstitial with no route back to the instructions, so sending it
    // straight to HTTPS strands it exactly when it needs help most. The install
    // page hands it on to HTTPS once the trust check passes.
    let pairing_url = match &prepared {
        Some(_) => auth::with_token(&format!("{base_url}/install"), &token),
        None => auth::with_token(&base_url, &token),
    };

    // mDNS gives the phone an address that survives DHCP. Windows' own
    // responder cannot be used for this — measured, it answers with a virtual
    // adapter's address — so the bridge runs its own. See `mdns`.
    //
    // The responder follows the address: a certificate that moved while the
    // name did not would leave `coyote.local` stable and wrong, which is worse
    // than unstable and right because nothing in the failure points at DNS.
    let (advertised_tx, advertised_rx) = tokio::sync::watch::channel(advertised_ip);
    let advertise = tls::Advertise {
        pinned: args.advertise,
        mdns_tx: Some(advertised_tx),
    };
    // Registered here, but `follow` needs a reactor, so the watcher is spawned
    // once the runtime exists.
    let mut mdns_follow = None;
    if prepared.is_some() {
        if let Some(responder) = mdns::start(advertised_ip, args.https_port) {
            mdns_follow = Some((responder, advertised_rx));
        }
    }

    // Origins the phone can legitimately present once TLS is in play. Omitting
    // the HTTPS ones means the app loads and is then refused its own WebSocket.
    let allowed_hosts = tls::browser_origins(Some(advertised_ip), args.http_port, args.https_port);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    let bind_addr = SocketAddr::new(args.bind, args.http_port);
    let player_endpoint = args.player.clone();
    let static_dir = args.static_dir.clone();
    let library_dir = args.library_dir.clone();
    let base_for_http = base_url.clone();
    let token_for_http = token.clone();
    let https_bind = args.bind;
    let https_port = args.https_port;
    let prepared_tls = prepared.is_some();
    let tls_public = prepared.as_ref().map(|p| Arc::clone(&p.public));
    let tls_start = prepared.map(|p| (p.ca, p.material));

    runtime.spawn(async move {
        if let Some((responder, rx)) = mdns_follow.take() {
            mdns::follow(responder, rx);
        }
        // The headless binary connects on start and never stops trying: it is
        // a service, and there is nobody here to press a button. The
        // supervisor's Connect/Disconnect commands exist for the UI, and are
        // simply left unused.
        let bridge = supervisor::spawn(Some(player_endpoint), Tap::disabled());

        // Spawned inside the runtime, because the poller is a tokio task. A
        // directory that does not exist yet is not refused here: the poller
        // picks it up when it appears, which is what a network share that
        // mounts after login needs.
        let library = library_dir.map(coyote_bridge::library::Library::spawn);

        match TcpListener::bind(bind_addr).await {
            Ok(listener) => {
                log_info!("[main] serving on http://{bind_addr}");
                // Bind the TLS listener *before* building the context, so
                // `ctx.tls` describes a listener that exists rather than one we
                // intended. Getting this backwards is not a cosmetic bug: the
                // install page's trust check reports a stage-2 failure as
                // "almost always means you missed the trust step", so a port
                // that failed to bind would send the user to reinstall a
                // perfectly good certificate, repeatedly, with no way to find
                // out otherwise. A missing listener must present as "HTTPS is
                // not running", not as a certificate fault.
                let https = match tls_start {
                    Some((ca, material)) => match tls::bind(https_bind, https_port).await {
                        Ok(listener) => Some((listener, ca, material)),
                        Err(e) => {
                            log_warn!(
                                "[main] {e}; serving plain HTTP only, so the phone cannot use \
                                 Bluetooth. The install page will say HTTPS is not running rather \
                                 than blaming the certificate."
                            );
                            None
                        }
                    },
                    None => None,
                };

                let ctx = Arc::new(http::Ctx {
                    snapshot_rx: bridge.snapshot_rx.clone(),
                    cmd_tx: bridge.cmd_tx.clone(),
                    static_dir,
                    library,
                    pairing_base: base_for_http,
                    token: std::sync::RwLock::new(token_for_http),
                    allowed_hosts,
                    on_token_rotated: None,
                    tls: https.is_some().then_some(tls_public).flatten(),
                });

                // Both listeners share one routing table and one context. Plain
                // HTTP stays up deliberately: it carries the install page, which
                // is the only thing a phone can reach before it trusts anything.
                if let Some((listener, ca, material)) = https {
                    log_info!("[main] serving TLS on https://{https_bind}:{https_port}");
                    let (certs_tx, certs_rx) = tokio::sync::watch::channel(material);
                    tokio::spawn(tls::keep_current(ca, certs_tx, advertise));
                    tokio::spawn(tls::run(listener, Arc::clone(&ctx), certs_rx));
                }

                http::run(listener, ctx).await;
            }
            Err(e) => {
                log_warn!("[main] could not bind {bind_addr}: {e}");
                std::process::exit(1);
            }
        }
    });

    log_info!("[main] player endpoint: {}", args.player);
    // Base URL only. The full pairing URL carries the token, and this log is
    // routinely pasted into bug reports.
    log_info!("[main] phone should open: {}", auth::redact_url(&pairing_url));
    log_info!(
        "[main] that URL carries a pairing token, withheld here because this log \
         gets shared. Open {local_url}/pair to see the full URL and its QR."
    );
    log_info!("[main] pairing QR: {local_url}/pair");
    if prepared_tls {
        // Deliberately about the *certificate*, not the listener. Whether the
        // TLS port actually binds is only known inside the runtime, and a line
        // here announcing "TLS is on" would keep saying so after a bind failure
        // — which is the same lie the install page was just fixed not to tell.
        // The listener logs itself when it comes up.
        log_info!(
            "[main] certificate ready; once HTTPS is listening the phone installs it \
             from the install page and then reaches the app at https://{}:{}",
            coyote_bridge::certs::BRIDGE_HOSTNAME,
            args.https_port
        );
    }
    logging::flush_now();

    #[cfg(feature = "tray")]
    if args.tray {
        // Never returns.
        let status_url = auth::with_token(&format!("{local_url}/healthz"), &token);
        coyote_bridge::tray::run(pairing_url, local_url, status_url);
    }

    #[cfg(not(feature = "tray"))]
    {
        let _ = (args.tray, &pairing_url, &local_url);
    }

    // Headless: park the main thread. Ctrl-C ends the process.
    loop {
        std::thread::park();
    }
}
