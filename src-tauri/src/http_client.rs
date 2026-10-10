//! Shared HTTP policy and status errors that retain the server's response body.
use std::time::Duration;
use ureq::ResponseExt as _;

pub type Response = ureq::http::Response<ureq::Body>;

/// Keep the v2 defaults explicit, particularly its refusal to use proxy env vars.
macro_rules! config_builder {
    () => {
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(5)
            .max_redirects_will_error(true)
            .save_redirect_history(true)
            .timeout_connect(Some(std::time::Duration::from_secs(30)))
            .http_status_as_error(false)
    };
}
pub(crate) use config_builder;

pub fn agent() -> ureq::Agent {
    ureq::Agent::with_parts(
        config_builder!().build(),
        ConnectPhaseConnector,
        DirectResolver,
    )
}

/// Remember connection failures before HTTP dispatch, when retry is safe.
#[derive(Debug)]
struct ConnectPhaseConnector;

#[derive(Debug)]
struct ConnectFailure(ureq::Error);

impl std::fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for ConnectFailure {}

pub(crate) fn connect_error(error: &ureq::Error) -> Option<&ureq::Error> {
    match error {
        ureq::Error::Other(error) => error.downcast_ref::<ConnectFailure>().map(|error| &error.0),
        _ => None,
    }
}

impl ureq::unversioned::transport::Connector for ConnectPhaseConnector {
    type Out = MetadataTransport;
    fn connect(
        &self,
        details: &ureq::unversioned::transport::ConnectionDetails,
        chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        use ureq::unversioned::transport::{
            ConnectionDetails, Connector as _, RustlsConnector, TcpConnector,
        };
        // v2 gives the configured TCP connect budget precedence over the
        // request deadline. TLS and HTTP I/O still use the request deadline.
        let tcp_details = ConnectionDetails {
            uri: details.uri,
            addrs: details.addrs.clone(),
            config: details.config,
            request_level: details.request_level,
            resolver: details.resolver,
            now: (details.current_time)(),
            timeout: socket_connect_timeout(details.config, details.timeout),
            current_time: details.current_time.clone(),
            run_connector: details.run_connector.clone(),
        };
        let connect_started = std::time::Instant::now();
        TcpConnector::default()
            .connect(&tcp_details, chained)
            .and_then(|tcp| {
                let elapsed = connect_started.elapsed();
                let mut tls_details = ConnectionDetails {
                    addrs: details.addrs.clone(),
                    current_time: details.current_time.clone(),
                    run_connector: details.run_connector.clone(),
                    ..*details
                };
                if let ureq::unversioned::transport::time::Duration::Exact(budget) =
                    details.timeout.after
                {
                    let remaining = budget.saturating_sub(elapsed);
                    if remaining.is_zero() && details.needs_tls() {
                        return Err(ureq::Error::Timeout(details.timeout.reason));
                    }
                    tls_details.timeout.after =
                        ureq::unversioned::transport::time::Duration::Exact(remaining);
                }
                RustlsConnector::default().connect(&tls_details, tcp)
            })
            .map(|transport| {
                transport.map(|inner| MetadataTransport {
                    inner: Box::new(inner),
                })
            })
            .map_err(|error| ureq::Error::Other(Box::new(ConnectFailure(error))))
    }
}

fn socket_connect_timeout(
    config: &ureq::config::Config,
    request: ureq::unversioned::transport::NextTimeout,
) -> ureq::unversioned::transport::NextTimeout {
    use ureq::unversioned::transport::{time, NextTimeout};
    config
        .timeouts()
        .connect
        .map_or(request, |connect| NextTimeout {
            after: time::Duration::Exact(connect),
            reason: ureq::Timeout::Connect,
        })
}

/// A full parser buffer is oversized metadata, not a disconnected peer.
/// This checks unconsumed input, never bytes transferred over the connection.
#[derive(Debug)]
struct MetadataTransport {
    inner: Box<dyn ureq::unversioned::transport::Transport>,
}

impl ureq::unversioned::transport::Transport for MetadataTransport {
    fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
        self.inner.buffers()
    }
    fn transmit_output(
        &mut self,
        amount: usize,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<(), ureq::Error> {
        self.inner.transmit_output(amount, timeout)
    }
    fn await_input(
        &mut self,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<bool, ureq::Error> {
        let buffers = self.inner.buffers();
        if !buffers.can_use_input()
            && !buffers.input().is_empty()
            && buffers.input_append_buf().is_empty()
        {
            return Err(ureq::Error::BodyStalled);
        }
        self.inner.await_input(timeout)
    }
    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }
    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

/// ureq 3 reports lookup failures as Io. Keep DNS failures distinct from
/// post-send I/O so daemon rendezvous and MCP retries retain their v2 behavior.
#[derive(Debug)]
struct DirectResolver;

impl ureq::unversioned::resolver::Resolver for DirectResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        ureq::unversioned::resolver::DefaultResolver::default()
            .resolve(uri, config, timeout)
            .map_err(|error| match error {
                ureq::Error::Io(_) => ureq::Error::HostNotFound,
                other => other,
            })
    }
}

pub(crate) fn screened_agent(
    config: ureq::config::Config,
    block_private: bool,
    screen: fn(&[std::net::SocketAddr], bool) -> std::io::Result<()>,
) -> ureq::Agent {
    ureq::Agent::with_parts(
        config,
        ConnectPhaseConnector,
        ScreenedResolver {
            block_private,
            screen,
        },
    )
}

/// ureq 3's StatusCode error discards the body. OAuth and Teams need it intact.
#[derive(Debug)]
pub enum Error {
    Status(u16, Response),
    Transport(ureq::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status(code, response) => {
                write!(f, "{}: status code {code}", response.get_uri())?;
                if let Some(history) = response.get_redirect_history().filter(|h| h.len() > 1) {
                    write!(f, " (redirected from {})", history[0])?;
                }
                Ok(())
            }
            Self::Transport(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

pub trait ResponseResultExt {
    fn retain_status_body(self) -> Result<Response, Error>;
}

impl ResponseResultExt for Result<Response, ureq::Error> {
    fn retain_status_body(self) -> Result<Response, Error> {
        let response = self.map_err(Error::Transport)?;
        if response.status().is_client_error() || response.status().is_server_error() {
            Err(Error::Status(response.status().as_u16(), response))
        } else {
            Ok(response)
        }
    }
}

/// Screen the entire DNS answer before ureq's fixed address array truncates it.
/// Literal IPs take this same path, including on redirects.
#[derive(Debug)]
pub(crate) struct ScreenedResolver {
    pub block_private: bool,
    pub screen: fn(&[std::net::SocketAddr], bool) -> std::io::Result<()>,
}

impl ureq::unversioned::resolver::Resolver for ScreenedResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        use std::net::ToSocketAddrs;
        use ureq::unversioned::resolver::DefaultResolver;
        let netloc = uri
            .scheme()
            .zip(uri.authority())
            .and_then(|(scheme, authority)| DefaultResolver::host_and_port(scheme, authority))
            .ok_or_else(|| ureq::Error::BadUri("missing host or port".into()))?;
        let addrs: Vec<_> = netloc
            .to_socket_addrs()
            .map_err(|_| ureq::Error::HostNotFound)?
            .collect();
        self.screen_addresses(addrs)
    }
}

impl ScreenedResolver {
    fn screen_addresses(
        &self,
        addrs: Vec<std::net::SocketAddr>,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        use ureq::unversioned::resolver::Resolver as _;
        (self.screen)(&addrs, self.block_private)?;
        if addrs.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        let mut resolved = self.empty();
        // ureq's fixed resolver array holds at most 16 candidates.
        for addr in addrs.into_iter().take(16) {
            resolved.push(addr);
        }
        Ok(resolved)
    }
}

/// v2's read/write socket timeouts reset on each I/O. A v3 body deadline would
/// close healthy long-lived subscriptions, so keep idle limits at the transport.
pub(crate) fn idle_agent(
    read: Duration,
    write: Option<Duration>,
    connect: Duration,
) -> ureq::Agent {
    use ureq::unversioned::transport::Connector;
    let config = config_builder!().timeout_connect(Some(connect)).build();
    ureq::Agent::with_parts(
        config,
        ConnectPhaseConnector.chain(IdleConnector { read, write }),
        DirectResolver,
    )
}

#[derive(Debug)]
struct IdleConnector {
    read: Duration,
    write: Option<Duration>,
}

impl<In: ureq::unversioned::transport::Transport> ureq::unversioned::transport::Connector<In>
    for IdleConnector
{
    type Out = IdleTransport<In>;
    fn connect(
        &self,
        _: &ureq::unversioned::transport::ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(|inner| IdleTransport {
            inner,
            read: self.read,
            write: self.write,
        }))
    }
}

#[derive(Debug)]
struct IdleTransport<In> {
    inner: In,
    read: Duration,
    write: Option<Duration>,
}

fn idle_timeout(
    mut timeout: ureq::unversioned::transport::NextTimeout,
    idle: Duration,
    reason: ureq::Timeout,
) -> ureq::unversioned::transport::NextTimeout {
    let idle = ureq::unversioned::transport::time::Duration::Exact(idle);
    if idle < timeout.after {
        timeout.after = idle;
        timeout.reason = reason;
    }
    timeout
}

impl<In: ureq::unversioned::transport::Transport> ureq::unversioned::transport::Transport
    for IdleTransport<In>
{
    fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
        self.inner.buffers()
    }
    fn transmit_output(
        &mut self,
        amount: usize,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<(), ureq::Error> {
        let timeout = self.write.map_or(timeout, |idle| {
            idle_timeout(timeout, idle, ureq::Timeout::SendBody)
        });
        self.inner.transmit_output(amount, timeout)
    }
    fn await_input(
        &mut self,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<bool, ureq::Error> {
        self.inner
            .await_input(idle_timeout(timeout, self.read, ureq::Timeout::RecvBody))
    }
    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }
    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

/// v2's set replaced existing values; http::Builder::header appends them.
pub trait RequestHeaderExt {
    fn set_header(self, name: &str, value: &str) -> Self;
}

impl<B> RequestHeaderExt for ureq::RequestBuilder<B> {
    fn set_header(mut self, name: &str, value: &str) -> Self {
        if let Some(headers) = self.headers_mut() {
            headers.remove(name);
        }
        self.header(name, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ureq::unversioned::resolver::Resolver as _;
    use ureq::unversioned::transport::{time, NextTimeout, Transport as _};

    #[test]
    fn http_defaults_preserve_direct_rustls_webpki_and_redirect_policy() {
        let config = config_builder!().build();
        assert!(config.proxy().is_none());
        assert_eq!(config.max_redirects(), 5);
        assert!(config.max_redirects_will_error());
        assert_eq!(config.timeouts().connect, Some(Duration::from_secs(30)));
        assert!(!config.http_status_as_error());
        assert_eq!(
            config.tls_config().provider(),
            ureq::tls::TlsProvider::Rustls
        );
        assert!(matches!(
            config.tls_config().root_certs(),
            ureq::tls::RootCerts::WebPki
        ));
    }

    #[test]
    fn socket_connect_budget_keeps_v2_precedence_over_request_deadlines() {
        let request = NextTimeout {
            after: time::Duration::Exact(Duration::from_secs(2)),
            reason: ureq::Timeout::Global,
        };
        let configured = socket_connect_timeout(&config_builder!().build(), request);
        assert_eq!(
            configured.after,
            time::Duration::Exact(Duration::from_secs(30))
        );
        assert_eq!(configured.reason, ureq::Timeout::Connect);
        let config = config_builder!()
            .timeout_connect(Some(Duration::from_secs(1)))
            .build();
        assert_eq!(
            socket_connect_timeout(&config, request).after,
            time::Duration::Exact(Duration::from_secs(1))
        );
        let config = config_builder!().timeout_connect(None).build();
        assert_eq!(
            socket_connect_timeout(&config, request).after,
            request.after
        );
        assert_eq!(
            socket_connect_timeout(&config, request).reason,
            request.reason
        );
    }

    #[test]
    fn proxy_environment_is_ignored() {
        if std::env::var_os("TOOLPORT_HTTP_PROXY_TEST").is_some() {
            assert!(agent().config().proxy().is_none());
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "http_client::tests::proxy_environment_is_ignored",
                "--nocapture",
            ])
            .env("TOOLPORT_HTTP_PROXY_TEST", "1")
            .envs(
                [
                    "HTTP_PROXY",
                    "HTTPS_PROXY",
                    "ALL_PROXY",
                    "http_proxy",
                    "https_proxy",
                    "all_proxy",
                ]
                .map(|name| (name, "http://127.0.0.1:9")),
            )
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn resolver_screens_the_private_tail_before_truncating_candidates() {
        let resolver = ScreenedResolver {
            block_private: true,
            screen: crate::oauth::screen_addrs,
        };
        let mut addresses = vec!["8.8.8.8:443".parse().unwrap(); 16];
        addresses.push("127.0.0.1:443".parse().unwrap());
        assert!(resolver.screen_addresses(addresses).is_err());
        assert!(resolver.screen_addresses(Vec::new()).is_err());
    }

    #[test]
    fn refused_connections_keep_the_pre_dispatch_error_phase() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let error = agent()
            .get(&url)
            .config()
            .timeout_global(Some(Duration::from_secs(3)))
            .build()
            .call()
            .unwrap_err();
        assert!(
            matches!(connect_error(&error), Some(ureq::Error::Io(io)) if io.kind() == std::io::ErrorKind::ConnectionRefused)
        );
    }

    #[test]
    fn replacement_headers_do_not_duplicate_authorization() {
        let request = agent()
            .post("http://localhost/")
            .set_header("Authorization", "Bearer old")
            .set_header("authorization", "Bearer new");
        let headers = request.headers_ref().unwrap();
        assert_eq!(headers.get_all("authorization").iter().count(), 1);
        assert_eq!(headers["authorization"], "Bearer new");
    }

    #[test]
    fn status_errors_keep_the_response_body_and_text() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", server.server_addr());
        let worker = std::thread::spawn(move || {
            server
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap()
                .respond(tiny_http::Response::from_string("provider detail").with_status_code(401))
                .unwrap();
        });
        let error = agent().get(&url).call().retain_status_body().unwrap_err();
        assert_eq!(error.to_string(), format!("{url}: status code 401"));
        let Error::Status(401, response) = error else {
            panic!("wrong error");
        };
        assert_eq!(
            response.into_body().read_to_string().unwrap(),
            "provider detail"
        );
        worker.join().unwrap();
    }

    #[test]
    fn gzip_json_is_still_decoded() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", server.server_addr());
        let worker = std::thread::spawn(move || {
            let request = server
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap();
            assert!(request
                .headers()
                .iter()
                .any(|header| header.field.equiv("Accept-Encoding")
                    && header.value.as_str().contains("gzip")));
            let body = vec![
                31, 139, 8, 0, 0, 0, 0, 0, 2, 3, 171, 86, 202, 207, 86, 178, 42, 41, 42, 77, 173,
                5, 0, 144, 95, 212, 167, 11, 0, 0, 0,
            ];
            request
                .respond(
                    tiny_http::Response::from_data(body)
                        .with_header(
                            tiny_http::Header::from_bytes("Content-Encoding", "gzip").unwrap(),
                        )
                        .with_header(
                            tiny_http::Header::from_bytes("Content-Type", "application/json")
                                .unwrap(),
                        ),
                )
                .unwrap();
        });
        let value: serde_json::Value = agent()
            .get(&url)
            .call()
            .unwrap()
            .into_body()
            .with_config()
            .limit(u64::MAX)
            .read_json()
            .unwrap();
        assert_eq!(value["ok"], true);
        worker.join().unwrap();
    }

    #[test]
    fn screened_resolver_checks_ipv4_ipv6_and_configured_local_endpoints() {
        let config = config_builder!().build();
        let timeout = NextTimeout {
            after: time::Duration::NotHappening,
            reason: ureq::Timeout::Resolve,
        };
        for url in ["http://127.0.0.1/", "http://[::1]/"] {
            let uri = url.parse().unwrap();
            assert!(ScreenedResolver {
                block_private: true,
                screen: crate::oauth::screen_addrs
            }
            .resolve(&uri, &config, timeout)
            .is_err());
            assert!(!ScreenedResolver {
                block_private: false,
                screen: crate::oauth::screen_addrs
            }
            .resolve(&uri, &config, timeout)
            .unwrap()
            .is_empty());
        }
        let uri = "http://169.254.169.254/".parse().unwrap();
        assert!(ScreenedResolver {
            block_private: false,
            screen: crate::oauth::screen_addrs
        }
        .resolve(&uri, &config, timeout)
        .is_err());
    }

    #[test]
    fn subscription_idle_limits_reset_per_io_and_keep_shorter_deadlines() {
        #[derive(Debug)]
        struct Socket;
        impl ureq::unversioned::transport::Transport for Socket {
            fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
                unreachable!()
            }
            fn transmit_output(&mut self, _: usize, _: NextTimeout) -> Result<(), ureq::Error> {
                unreachable!()
            }
            fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
                assert_eq!(timeout.after, time::Duration::from_secs(90));
                Ok(true)
            }
            fn is_open(&mut self) -> bool {
                true
            }
        }
        let agent = idle_agent(Duration::from_secs(90), None, Duration::from_secs(2));
        assert_eq!(agent.config().timeouts().recv_body, None);
        let mut transport = IdleTransport {
            inner: Socket,
            read: Duration::from_secs(90),
            write: None,
        };
        let unlimited = NextTimeout {
            after: time::Duration::NotHappening,
            reason: ureq::Timeout::Global,
        };
        for _ in 0..100 {
            assert!(transport.await_input(unlimited).unwrap());
        }
        let deadline = NextTimeout {
            after: time::Duration::from_secs(1),
            reason: ureq::Timeout::Global,
        };
        assert_eq!(
            idle_timeout(deadline, Duration::from_secs(90), ureq::Timeout::RecvBody),
            deadline
        );
    }
}
