//! Per-provider SSH dynamic-forward transport.
//!
//! An optional provider tunnel is an `ssh -N -D` child bound exclusively to a
//! random loopback port. Requests use SOCKS5h so destination DNS is resolved
//! at the SSH egress, not on the router host. The SSH child gets an explicit
//! `SSH_AUTH_SOCK`; no shell is involved in parsing user-provided extra flags.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{ProviderConfig, SshProxyConfig};

#[derive(Default)]
pub struct ProviderClients {
    tunnels: Mutex<HashMap<SshProxyConfig, Arc<SshTunnel>>>,
}

impl ProviderClients {
    pub fn client(&self, provider: &ProviderConfig) -> Result<reqwest::Client, String> {
        let Some(ssh) = &provider.ssh_proxy else {
            return Ok(reqwest::Client::new());
        };
        let tunnel = {
            let mut tunnels = self.tunnels.lock().map_err(|_| "SSH tunnel registry is poisoned")?;
            tunnels
                .entry(ssh.clone())
                .or_insert_with(|| Arc::new(SshTunnel::new(ssh.clone())))
                .clone()
        };
        let addr = tunnel.ensure()?;
        reqwest::Client::builder()
            // `h` deliberately proxies DNS through the remote network too.
            .proxy(reqwest::Proxy::all(format!("socks5h://{addr}")).map_err(|err| err.to_string())?)
            .connect_timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(60))
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| format!("could not build SSH-proxied client: {err}"))
    }
}

struct SshTunnel {
    config: SshProxyConfig,
    state: Mutex<Option<RunningTunnel>>,
}

struct RunningTunnel {
    child: Child,
    addr: SocketAddr,
}

impl SshTunnel {
    fn new(config: SshProxyConfig) -> Self {
        Self { config, state: Mutex::new(None) }
    }

    fn ensure(&self) -> Result<SocketAddr, String> {
        let mut state = self.state.lock().map_err(|_| "SSH tunnel state is poisoned")?;
        if let Some(running) = state.as_mut() {
            if running.child.try_wait().map_err(|err| err.to_string())?.is_none() && reachable(running.addr) {
                return Ok(running.addr);
            }
            let _ = running.child.kill();
            let _ = running.child.wait();
            *state = None;
        }
        let running = self.start()?;
        let addr = running.addr;
        *state = Some(running);
        Ok(addr)
    }

    fn start(&self) -> Result<RunningTunnel, String> {
        let extra = shell_words::split(&self.config.extra_flags)
            .map_err(|err| format!("invalid SSH extra_flags: {err}"))?;
        reject_conflicting_flags(&extra)?;
        let addr = reserve_loopback_port()?;
        let mut command = Command::new("ssh");
        command
            .args(extra)
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ExitOnForwardFailure=yes")
            .arg("-N")
            .arg("-D")
            .arg(addr.to_string())
            .arg(&self.config.destination)
            .env("SSH_AUTH_SOCK", &self.config.agent_socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|err| {
            format!(
                "could not start SSH proxy for '{}': {err}",
                self.config.destination
            )
        })?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if reachable(addr) {
                tracing::info!(destination = self.config.destination, %addr, "SSH provider proxy ready");
                return Ok(RunningTunnel { child, addr });
            }
            if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
                let detail = child
                    .stderr
                    .as_mut()
                    .and_then(|stderr| std::io::Read::read_to_end(stderr, &mut Vec::new()).ok())
                    .map(|_| " (see SSH stderr)")
                    .unwrap_or("");
                return Err(format!(
                    "SSH proxy for '{}' exited before becoming ready ({status}){detail}",
                    self.config.destination
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err(format!(
            "SSH proxy for '{}' did not open its SOCKS listener within 10 seconds",
            self.config.destination
        ))
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(running) = state.as_mut() {
                let _ = running.child.kill();
                let _ = running.child.wait();
            }
        }
    }
}

fn reserve_loopback_port() -> Result<SocketAddr, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    let addr = listener.local_addr().map_err(|err| err.to_string())?;
    drop(listener);
    Ok(addr)
}

fn reachable(addr: SocketAddr) -> bool {
    TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_ok()
}

/// These alter or replace the dynamic forward the router owns. Accept normal
/// SSH options such as `-J`, `-p`, `-i`, `-F`, and `-o ServerAliveInterval=…`.
fn reject_conflicting_flags(args: &[String]) -> Result<(), String> {
    for (index, arg) in args.iter().enumerate() {
        if matches!(arg.as_str(), "-D" | "-L" | "-R" | "-W" | "-S" | "-O" | "-N") {
            return Err(format!("SSH extra_flags must not include {arg}; the router owns tunnel setup"));
        }
        if arg == "-o" {
            if let Some(value) = args.get(index + 1) {
                let key = value.split('=').next().unwrap_or_default();
                if matches!(key, "ControlPath" | "DynamicForward" | "LocalForward" | "RemoteForward") {
                    return Err(format!("SSH extra_flags must not override {key}"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_ordinary_ssh_flags() {
        assert!(reject_conflicting_flags(&["-J".into(), "relay.example".into(), "-p".into(), "2222".into()]).is_ok());
    }

    #[test]
    fn rejects_forward_override_flags() {
        assert!(reject_conflicting_flags(&["-D".into(), "9000".into()]).is_err());
        assert!(reject_conflicting_flags(&["-o".into(), "ControlPath=/tmp/socket".into()]).is_err());
    }
}
