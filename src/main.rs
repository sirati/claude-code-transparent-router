use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use claude_code_transparent_router::{config, idle, tui, AppState};
use listenfd::ListenFd;
use tokio::net::TcpListener;
use tokio::sync::watch;

struct Args {
    config: Option<PathBuf>,
    daemon: bool,
    supervisor: bool,
    worker: bool,
    reload: bool,
    target: Option<PathBuf>,
    user_config: bool,
    listen: Option<std::net::SocketAddr>,
    idle_timeout: Option<u64>,
    command: Option<(String, String)>,
}

fn parse_args() -> Args {
    let mut args = Args {
        config: None,
        daemon: false,
        supervisor: false,
        worker: false,
        reload: false,
        target: None,
        user_config: false,
        listen: None,
        idle_timeout: None,
        command: None,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--config" => args.config = argv.next().map(PathBuf::from),
            "--daemon" => args.daemon = true,
            "--supervisor" => args.supervisor = true,
            "--worker" => args.worker = true,
            "--reload" => args.reload = true,
            "--target" => args.target = argv.next().map(PathBuf::from),
            "--user-config" => args.user_config = true,
            "--listen" => {
                args.listen = argv.next().and_then(|addr| addr.parse().ok());
                if args.listen.is_none() {
                    eprintln!("--listen needs an address like 127.0.0.1:8787");
                    std::process::exit(2);
                }
            }
            "--idle-timeout" => {
                args.idle_timeout = argv.next().and_then(|secs| secs.parse().ok());
                if args.idle_timeout.is_none() {
                    eprintln!("--idle-timeout needs a number of seconds (0 disables)");
                    std::process::exit(2);
                }
            }
            "login" | "logout" => match argv.next() {
                Some(provider) => args.command = Some((arg, provider)),
                None => {
                    eprintln!("{arg} needs a provider name");
                    std::process::exit(2);
                }
            },
            "--help" | "-h" => {
                println!(
                    "claude-router [--config <path>] [--daemon] [--listen <addr>]\n\
                     \x20            [--user-config] [--idle-timeout <seconds>]\n\
                     claude-router login <provider>\n\
                     claude-router logout <provider>\n\n\
                     From a terminal: opens the TUI, which configures the running daemon.\n\
                     --daemon (or no TTY): runs the daemon itself.\n\
                     --supervisor owns socket activation and performs worker handovers.\n\
                     --reload asks the running supervisor to reload or hand over."
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}

fn main() {
    let args = parse_args();
    if args.reload {
        let code = request_reload(&args);
        std::process::exit(code);
    }
    let config = match config::Config::load(args.config.clone()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("config error: {err}");
            std::process::exit(1);
        }
    };
    if let Some((command, provider)) = args.command {
        let code = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(run_auth_command(&config, &command, &provider));
        std::process::exit(code);
    }
    if !args.daemon && !args.supervisor && !args.worker && std::io::stdout().is_terminal() {
        if let Err(err) = tui::run(Arc::new(config)) {
            eprintln!("tui error: {err}");
            std::process::exit(1);
        }
        return;
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "claude_code_transparent_router=info,claude_router=info".into()),
        )
        .init();
    tokio::runtime::Builder::new_multi_thread()
        // The router is I/O-bound. One runtime worker per host CPU wastes
        // memory on high-core systems and makes it an attractive OOM victim.
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(async move {
            if args.supervisor {
                supervise(config, args).await;
            } else {
                serve(config, args).await;
            }
        });
}

fn control_path() -> PathBuf {
    std::env::var_os("CLAUDE_ROUTER_CONTROL_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("claude-router/control.sock")
        })
}

/// Ask the resident supervisor to apply a new config, or hand its listener to
/// a new worker executable. The command only ever talks through a 0700 runtime
/// directory owned by the same user; it never opens a TCP control endpoint.
fn request_reload(args: &Args) -> i32 {
    let config = args.config.as_ref().map_or_else(|| "-".into(), |p| p.display().to_string());
    let target = args.target.as_ref().map_or_else(|| std::env::current_exe().unwrap().display().to_string(), |p| p.display().to_string());
    let Ok(mut stream) = UnixStream::connect(control_path()) else {
        eprintln!("claude-router supervisor is not running");
        return 1;
    };
    if writeln!(
        stream,
        "reload\t{target}\t{config}\t{}\t{}",
        args.user_config as u8,
        args.idle_timeout.map_or_else(|| "-".into(), |timeout| timeout.to_string()),
    )
    .is_err() {
        eprintln!("could not send reload request");
        return 1;
    }
    let mut reply = String::new();
    if BufReader::new(stream).read_line(&mut reply).is_err() || !reply.starts_with("ok") {
        eprintln!("router reload failed: {}", reply.trim());
        return 1;
    }
    0
}

async fn supervise(config: config::Config, args: Args) {
    let listener = take_listener(&config, &args).await;
    let path = control_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("control directory");
        std::fs::set_permissions(parent, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .expect("control directory permissions");
    }
    let _ = std::fs::remove_file(&path);
    let control = UnixListener::bind(&path).expect("bind supervisor control socket");
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .expect("control socket permissions");

    let mut spec = WorkerSpec::from_args(&args);
    let mut worker = spawn_worker(&listener, &spec).expect("start initial router worker");
    send_worker(&mut worker, "serve").expect("activate initial router worker");
    let mut draining_workers = Vec::new();
    tracing::info!(worker = worker.id(), "router worker ready");
    loop {
        control.set_nonblocking(true).expect("nonblocking control socket");
        match control.accept() {
            Ok((mut request, _)) => {
                let mut line = String::new();
                BufReader::new(request.try_clone().expect("clone control socket"))
                    .read_line(&mut line)
                    .expect("read control request");
                match ReloadRequest::parse(&line) {
                    Some(next) if next.target == spec.target => {
                        let reload = match &next.config {
                            Some(path) => format!("reload\t{}", path.display()),
                            None => "reload\t-".into(),
                        };
                        if send_worker(&mut worker, &reload).is_ok() {
                            spec = next.into();
                            let _ = writeln!(request, "ok");
                        } else {
                            let _ = writeln!(request, "error worker reload failed");
                        }
                    }
                    Some(next) => {
                        let next_spec = WorkerSpec::from(next);
                        match spawn_worker(&listener, &next_spec) {
                            Ok(mut successor) => {
                                // The successor has parsed its config and owns a duplicated listener,
                                // but cannot accept until the old worker has closed its accept loop.
                                if send_worker(&mut worker, "drain").is_ok()
                                    && send_worker(&mut successor, "serve").is_ok()
                                {
                                    draining_workers.push(worker);
                                    worker = successor;
                                    spec = next_spec;
                                    let _ = writeln!(request, "ok");
                                } else {
                                    let _ = writeln!(request, "error handover failed");
                                }
                            }
                            Err(err) => {
                                let _ = writeln!(request, "error {err}");
                            }
                        }
                    }
                    None => {
                        let _ = writeln!(request, "error malformed request");
                    }
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => tracing::warn!(%err, "control socket accept failed"),
        }
        if let Ok(Some(status)) = worker.child.try_wait() {
            tracing::warn!(?status, "router worker exited; starting replacement");
            match spawn_worker(&listener, &spec) {
                Ok(next) => worker = next,
                Err(err) => tracing::error!(%err, "could not restart router worker"),
            }
        }
        for old in &mut draining_workers {
            let _ = old.child.try_wait();
        }
        draining_workers.retain_mut(|old| old.child.try_wait().ok().flatten().is_none());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct WorkerSpec {
    target: PathBuf,
    config: Option<PathBuf>,
    user_config: bool,
    idle_timeout: Option<u64>,
}

impl WorkerSpec {
    fn from_args(args: &Args) -> Self {
        Self {
            target: std::env::current_exe().expect("current router executable"),
            config: args.config.clone(),
            user_config: args.user_config,
            idle_timeout: args.idle_timeout,
        }
    }
}

struct ReloadRequest {
    target: PathBuf,
    config: Option<PathBuf>,
    user_config: bool,
    idle_timeout: Option<u64>,
}

impl ReloadRequest {
    fn parse(line: &str) -> Option<Self> {
        let mut fields = line.trim_end().split('\t');
        (fields.next()? == "reload").then_some(())?;
        let target = PathBuf::from(fields.next()?);
        let config = match fields.next()? {
            "-" => None,
            path => Some(PathBuf::from(path)),
        };
        let user_config = fields.next()? == "1";
        let idle_timeout = match fields.next()? {
            "-" => None,
            value => Some(value.parse().ok()?),
        };
        Some(Self { target, config, user_config, idle_timeout })
    }
}

impl From<ReloadRequest> for WorkerSpec {
    fn from(value: ReloadRequest) -> Self {
        Self {
            target: value.target,
            config: value.config,
            user_config: value.user_config,
            idle_timeout: value.idle_timeout,
        }
    }
}

fn set_cloexec(fd: i32, enabled: bool) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let flags = if enabled { flags | libc::FD_CLOEXEC } else { flags & !libc::FD_CLOEXEC };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

struct Worker {
    child: Child,
    control: UnixStream,
}

impl Worker {
    fn id(&self) -> u32 {
        self.child.id()
    }
}

fn spawn_worker(listener: &std::net::TcpListener, spec: &WorkerSpec) -> Result<Worker, String> {
    let child_listener = listener.try_clone().map_err(|err| err.to_string())?;
    let (parent, child) = UnixStream::pair().map_err(|err| err.to_string())?;
    let listener_fd = child_listener.as_raw_fd();
    let control_fd = child.as_raw_fd();
    set_cloexec(listener_fd, false).map_err(|err| err.to_string())?;
    set_cloexec(control_fd, false).map_err(|err| err.to_string())?;
    let mut command = Command::new(&spec.target);
    command
        .arg("--worker")
        .env("CLAUDE_ROUTER_LISTENER_FD", listener_fd.to_string())
        .env("CLAUDE_ROUTER_WORKER_CONTROL_FD", control_fd.to_string());
    if let Some(config) = &spec.config {
        command.arg("--config").arg(config);
    }
    if spec.user_config {
        command.arg("--user-config");
    }
    if let Some(timeout) = spec.idle_timeout {
        command.arg("--idle-timeout").arg(timeout.to_string());
    }
    let child_process = command.spawn().map_err(|err| err.to_string())?;
    set_cloexec(listener_fd, true).map_err(|err| err.to_string())?;
    set_cloexec(control_fd, true).map_err(|err| err.to_string())?;
    drop(child_listener);
    drop(child);
    let mut reader = BufReader::new(parent.try_clone().map_err(|err| err.to_string())?);
    let mut ready = String::new();
    reader.read_line(&mut ready).map_err(|err| err.to_string())?;
    if ready != "ready\n" {
        return Err(format!("worker did not become ready: {}", ready.trim()));
    }
    Ok(Worker { child: child_process, control: parent })
}

fn send_worker(worker: &mut Worker, command: &str) -> std::io::Result<()> {
    writeln!(worker.control, "{command}")?;
    worker.control.flush()?;
    let mut reply = String::new();
    BufReader::new(worker.control.try_clone()?).read_line(&mut reply)?;
    if reply == "ok\n" { Ok(()) } else { Err(std::io::Error::other(reply)) }
}

async fn take_listener(config: &config::Config, args: &Args) -> std::net::TcpListener {
    match ListenFd::from_env().take_tcp_listener(0) {
        Ok(Some(listener)) => listener,
        _ => std::net::TcpListener::bind(args.listen.unwrap_or(config.listen))
            .expect("router listener bind"),
    }
}

async fn serve(config: config::Config, args: Args) {
    let listener = if args.worker {
        let fd: i32 = std::env::var("CLAUDE_ROUTER_LISTENER_FD")
            .expect("worker listener fd")
            .parse()
            .expect("numeric worker listener fd");
        unsafe { std::net::TcpListener::from_raw_fd(fd) }
    } else {
        take_listener(&config, &args).await
    };
    listener.set_nonblocking(true).expect("nonblocking listener");
    let listener = TcpListener::from_std(listener).expect("tokio listener");
    let bound = listener.local_addr().expect("local addr");
    let allowed = config.allowed_uids.clone();
    let config_path = args.config.clone();
    let config = Arc::new(ArcSwap::from_pointee(config));
    let activity = idle::Activity::new();
    let drain = idle::Drain::new();
    let (serve_tx, serve_rx) = watch::channel(!args.worker);
    let (drain_tx, drain_rx) = watch::channel(false);
    let worker_control = args.worker.then(|| {
        let fd: i32 = std::env::var("CLAUDE_ROUTER_WORKER_CONTROL_FD")
            .expect("worker control fd")
            .parse()
            .expect("numeric worker control fd");
        unsafe { UnixStream::from_raw_fd(fd) }
    });
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(60))
        .no_gzip().no_brotli().no_deflate().no_zstd()
        .redirect(reqwest::redirect::Policy::none())
        .build().expect("reqwest client");
    let user_configs = (args.user_config || config.load().user_config).then(|| {
        Arc::new(claude_code_transparent_router::user_config::UserConfigs::new(config.load_full()))
    });
    let state = AppState {
        client,
        provider_clients: Arc::new(claude_code_transparent_router::ssh_proxy::ProviderClients::default()),
        config: config.clone(),
        user_configs,
        listen: bound,
        compact_override: Default::default(),
    };
    if let Some(stream) = worker_control {
        start_worker_control(stream, config.clone(), config_path.clone(), drain.clone(), serve_tx, drain_tx);
    }
    if !*serve_rx.borrow() {
        // A supervisor can disappear while this worker is waiting to be
        // activated (for example after an OOM kill). Exiting cleanly lets the
        // systemd supervisor restart instead of turning a control-channel EOF
        // into a panic and a restart storm.
        if serve_rx.clone().changed().await.is_err() {
            tracing::warn!("supervisor exited before activating router worker");
            return;
        }
    }
    tracing::info!(addr = %bound, "claude-router worker serving");
    let idle_timeout = args.idle_timeout.or(config.load().idle_timeout_secs).filter(|secs| *secs > 0).map(Duration::from_secs);
    let shutdown_drain = drain.clone();
    let idle_activity = activity.clone();
    let idle_wait = async move {
        match idle_timeout {
            Some(timeout) => idle_activity.wait_until_idle(timeout).await,
            None => std::future::pending::<()>().await,
        }
    };
    let shutdown = async move {
        tokio::select! {
            _ = shutdown_signal() => shutdown_drain.begin(),
            _ = async { drain_rx.clone().changed().await.ok(); } => {},
            _ = idle_wait => {},
        }
    };
    axum::serve(
        claude_code_transparent_router::peer::UidFiltered::new(listener, allowed),
        claude_code_transparent_router::app_with_activity(state, activity, drain)
            .into_make_service_with_connect_info::<claude_code_transparent_router::peer::PeerInfo>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .expect("serve");
}

fn start_worker_control(
    stream: UnixStream,
    config: Arc<ArcSwap<config::Config>>,
    config_path: Option<PathBuf>,
    drain: idle::Drain,
    serve_tx: watch::Sender<bool>,
    drain_tx: watch::Sender<bool>,
) {
    std::thread::spawn(move || {
        let mut writer = stream.try_clone().expect("clone worker control");
        let reader = BufReader::new(stream);
        let _ = writeln!(writer, "ready");
        for line in reader.lines() {
            match line.as_deref() {
                Ok("serve") => { let _ = serve_tx.send(true); let _ = writeln!(writer, "ok"); }
                Ok("drain") => {
                    drain.begin();
                    let _ = drain_tx.send(true);
                    let _ = writeln!(writer, "ok");
                }
                Ok(command) if command == "reload" || command.starts_with("reload\t") => {
                    let path = command
                        .split_once('\t')
                        .and_then(|(_, path)| (path != "-").then(|| PathBuf::from(path)))
                        .or_else(|| config_path.clone());
                    match config::Config::load(path) {
                        Ok(next) => {
                            config.store(Arc::new(next));
                            let _ = writeln!(writer, "ok");
                        }
                        Err(err) => {
                            let _ = writeln!(writer, "error {err}");
                        }
                    }
                }
                _ => { let _ = writeln!(writer, "error unknown command"); }
            }
            let _ = writer.flush();
        }
    });
}

async fn run_auth_command(config: &config::Config, command: &str, provider_name: &str) -> i32 {
    let Some(provider) = config.providers.iter().find(|p| p.name == provider_name) else {
        eprintln!("no provider named '{provider_name}' in the config");
        return 1;
    };
    let Some(oauth_config) = provider.oauth.as_ref() else {
        eprintln!("provider '{provider_name}' uses an API key, not a login; set its credential in the TUI instead");
        return 1;
    };
    let tokens = claude_code_transparent_router::oauth::TokenStore::new(&config.credentials_dir);
    if command == "logout" {
        return match tokens.clear(provider_name) {
            Ok(()) => { println!("signed out of '{provider_name}'"); 0 }
            Err(err) => { eprintln!("could not clear the '{provider_name}' login: {err}"); 1 }
        };
    }
    let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build().expect("http client");
    let started = match claude_code_transparent_router::oauth::login::start(oauth_config).await {
        Ok(started) => started,
        Err(err) => { eprintln!("{err}"); return 1; }
    };
    println!("Sign in to '{provider_name}' at:\n\n{}\n", started.authorize_url);
    println!("Signing in on another machine? Paste the final localhost redirect URL here:");
    claude_code_transparent_router::oauth::login::open_browser(&started.authorize_url);
    let session = tokio::select! {
        result = started.complete(&client, oauth_config) => result,
        pasted = read_line() => match pasted { Some(url) => started.complete_from_url(&client, oauth_config, &url).await, None => Err("no redirect URL given".to_string()) },
    };
    let session = match session { Ok(session) => session, Err(err) => { eprintln!("login failed: {err}"); return 1; } };
    let daemon_base = format!("http://{}", config.listen);
    match claude_code_transparent_router::oauth::hand_to_daemon(&client, &daemon_base, provider_name, &session).await {
        Ok(()) => { println!("Signed in to '{provider_name}' ({}).", session.preview()); 0 }
        Err(daemon_err) => match tokens.save(provider_name, &session) {
            Ok(()) => { println!("Signed in to '{provider_name}' ({}), stored locally. The running daemon did not accept it ({daemon_err}).", session.preview()); 0 }
            Err(err) => { eprintln!("signed in, but could not save the tokens: {err}"); 1 }
        },
    }
}

async fn read_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok()?;
        Some(line.trim().to_string()).filter(|line| !line.is_empty())
    }).await.ok().flatten()
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
}
