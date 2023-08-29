use std::{
    ffi::CStr,
    io,
    net::{TcpStream, ToSocketAddrs},
    os::fd::{AsRawFd, IntoRawFd},
    pin::{pin, Pin},
    task::{Context, Poll},
    time::Duration,
};

use anyhow::bail;
use async_io::Async;
use embedded_svc::wifi::AuthMethod;
use esp_idf_hal::prelude::Peripherals;
use esp_idf_svc::{
    errors::EspIOError,
    eventloop::EspSystemEventLoop,
    tls::{self, AsyncEspTls, PollableSocket, Socket, X509},
    wifi::{BlockingWifi, EspWifi},
};
use esp_idf_sys::{self as _, EspError, ESP_FAIL};
use futures_lite::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, Future};
use log::*;

const CA_CERT: &str = "-----BEGIN CERTIFICATE-----
MIIEvjCCA6agAwIBAgIQBtjZBNVYQ0b2ii+nVCJ+xDANBgkqhkiG9w0BAQsFADBh
MQswCQYDVQQGEwJVUzEVMBMGA1UEChMMRGlnaUNlcnQgSW5jMRkwFwYDVQQLExB3
d3cuZGlnaWNlcnQuY29tMSAwHgYDVQQDExdEaWdpQ2VydCBHbG9iYWwgUm9vdCBD
QTAeFw0yMTA0MTQwMDAwMDBaFw0zMTA0MTMyMzU5NTlaME8xCzAJBgNVBAYTAlVT
MRUwEwYDVQQKEwxEaWdpQ2VydCBJbmMxKTAnBgNVBAMTIERpZ2lDZXJ0IFRMUyBS
U0EgU0hBMjU2IDIwMjAgQ0ExMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKC
AQEAwUuzZUdwvN1PWNvsnO3DZuUfMRNUrUpmRh8sCuxkB+Uu3Ny5CiDt3+PE0J6a
qXodgojlEVbbHp9YwlHnLDQNLtKS4VbL8Xlfs7uHyiUDe5pSQWYQYE9XE0nw6Ddn
g9/n00tnTCJRpt8OmRDtV1F0JuJ9x8piLhMbfyOIJVNvwTRYAIuE//i+p1hJInuW
raKImxW8oHzf6VGo1bDtN+I2tIJLYrVJmuzHZ9bjPvXj1hJeRPG/cUJ9WIQDgLGB
Afr5yjK7tI4nhyfFK3TUqNaX3sNk+crOU6JWvHgXjkkDKa77SU+kFbnO8lwZV21r
eacroicgE7XQPUDTITAHk+qZ9QIDAQABo4IBgjCCAX4wEgYDVR0TAQH/BAgwBgEB
/wIBADAdBgNVHQ4EFgQUt2ui6qiqhIx56rTaD5iyxZV2ufQwHwYDVR0jBBgwFoAU
A95QNVbRTLtm8KPiGxvDl7I90VUwDgYDVR0PAQH/BAQDAgGGMB0GA1UdJQQWMBQG
CCsGAQUFBwMBBggrBgEFBQcDAjB2BggrBgEFBQcBAQRqMGgwJAYIKwYBBQUHMAGG
GGh0dHA6Ly9vY3NwLmRpZ2ljZXJ0LmNvbTBABggrBgEFBQcwAoY0aHR0cDovL2Nh
Y2VydHMuZGlnaWNlcnQuY29tL0RpZ2lDZXJ0R2xvYmFsUm9vdENBLmNydDBCBgNV
HR8EOzA5MDegNaAzhjFodHRwOi8vY3JsMy5kaWdpY2VydC5jb20vRGlnaUNlcnRH
bG9iYWxSb290Q0EuY3JsMD0GA1UdIAQ2MDQwCwYJYIZIAYb9bAIBMAcGBWeBDAEB
MAgGBmeBDAECATAIBgZngQwBAgIwCAYGZ4EMAQIDMA0GCSqGSIb3DQEBCwUAA4IB
AQCAMs5eC91uWg0Kr+HWhMvAjvqFcO3aXbMM9yt1QP6FCvrzMXi3cEsaiVi6gL3z
ax3pfs8LulicWdSQ0/1s/dCYbbdxglvPbQtaCdB73sRD2Cqk3p5BJl+7j5nL3a7h
qG+fh/50tx8bIKuxT8b1Z11dmzzp/2n3YWzW2fP9NsarA4h20ksudYbj/NhVfSbC
EXffPgK2fPOre3qGNm+499iTcc+G33Mw+nur7SpZyEKEOxEXGlLzyQ4UfaJbcme6
ce1XR2bFuAJKZTRei9AqPCCcUZlM51Ke92sRKw2Sfh3oius2FkOH6ipjv3U/697E
A7sKPPcw7+uvTPyLNhBzPvOk
-----END CERTIFICATE-----\0";

pub struct AsyncTcp(Option<Async<TcpStream>>);

impl Socket for AsyncTcp {
    fn handle(&self) -> i32 {
        self.0.as_ref().unwrap().as_raw_fd()
    }

    fn release(&mut self) -> Result<(), esp_idf_sys::EspError> {
        let socket = self.0.take().unwrap();
        socket.into_inner().unwrap().into_raw_fd();

        Ok(())
    }
}

impl PollableSocket for AsyncTcp {
    fn poll_readable(
        &self,
        ctx: &mut std::task::Context,
    ) -> std::task::Poll<Result<(), esp_idf_sys::EspError>> {
        pin!(&mut self.0.as_ref().unwrap().readable())
            .poll(ctx)
            .map_err(|e| {
                log::error!("readable future returned error {e}");
                EspError::from_infallible::<ESP_FAIL>()
            })
    }

    fn poll_writable(
        &self,
        ctx: &mut std::task::Context,
    ) -> std::task::Poll<Result<(), esp_idf_sys::EspError>> {
        pin!(&mut self.0.as_ref().unwrap().writable())
            .poll(ctx)
            .map_err(|e| {
                log::error!("writable future returned error {e}");
                EspError::from_infallible::<ESP_FAIL>()
            })
    }
}

pub struct AsyncTls(pub AsyncEspTls<AsyncTcp>);

impl AsyncRead for AsyncTls {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        pin!(self.0.read(buf))
            .poll(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, EspIOError(e)))
    }
}

impl AsyncWrite for AsyncTls {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        pin!(self.0.write(buf))
            .poll(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, EspIOError(e)))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub async fn connect_async_tls(
    hostname: &str,
    port: u16,
    cfg: &esp_idf_svc::tls::Config<'_>,
) -> anyhow::Result<AsyncTls> {
    let tcp =
        Async::<TcpStream>::connect((hostname, port).to_socket_addrs()?.next().unwrap()).await?;
    let mut tls = AsyncEspTls::adopt(AsyncTcp(Some(tcp)))
        .map_err(|e| anyhow::anyhow!("failed to create EspTls: {e}"))?;
    log::info!("adopted async tcp stream");
    dbg!(tls.negotiate(hostname, cfg).await)?;

    Ok(AsyncTls(tls))
}

async fn get_request() -> anyhow::Result<()> {
    info!("Connecting tls...");
    let mut tls = connect_async_tls(
        "example.com",
        443,
        &tls::Config {
            ca_cert: Some(X509::pem(
                CStr::from_bytes_with_nul(CA_CERT.as_bytes()).unwrap(),
            )),
            common_name: Some("example.com"),
            timeout_ms: 0,
            ..Default::default()
        },
    )
    .await?;
    info!("Connected tls");
    tls.write_all(b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")
        .await?;
    info!("Wrote tls");
    async_io::Timer::after(Duration::from_secs(1)).await;
    let mut buf = [0; 1024];
    tls.read(&mut buf).await?;
    let s = String::from_utf8_lossy(&buf);
    info!("response:\n{s}");

    Ok(())
}

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_sys::link_patches();
    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take().unwrap();
    let sysloop = EspSystemEventLoop::take().unwrap();

    let _wifi = {
        let ssid = "ssid";
        let pass = "pass";

        let mut auth_method = AuthMethod::WPA2Personal;
        if ssid.is_empty() {
            bail!("Missing WiFi name");
        }
        if pass.is_empty() {
            auth_method = AuthMethod::None;
            info!("Wifi password is empty");
        }
        let mut esp_wifi = EspWifi::new(peripherals.modem, sysloop.clone(), None)?;

        let mut wifi = BlockingWifi::wrap(&mut esp_wifi, sysloop)?;

        wifi.set_configuration(&embedded_svc::wifi::Configuration::Client(
            embedded_svc::wifi::ClientConfiguration::default(),
        ))?;

        info!("Starting wifi...");

        wifi.start()?;

        info!("Scanning...");

        let ap_infos = wifi.scan()?;

        let ours = ap_infos.into_iter().find(|a| a.ssid == ssid);

        let channel = if let Some(ours) = ours {
            info!(
                "Found configured access point {} on channel {}",
                ssid, ours.channel
            );
            Some(ours.channel)
        } else {
            info!(
            "Configured access point {} not found during scanning, will go with unknown channel",
            ssid
        );
            None
        };

        wifi.set_configuration(&embedded_svc::wifi::Configuration::Client(
            embedded_svc::wifi::ClientConfiguration {
                ssid: ssid.into(),
                password: pass.into(),
                channel,
                auth_method,
                ..Default::default()
            },
        ))?;

        info!("Connecting wifi...");

        wifi.connect()?;

        info!("Waiting for DHCP lease...");

        wifi.wait_netif_up()?;

        let ip_info = wifi.wifi().sta_netif().get_ip_info()?;

        info!("Wifi STA DHCP info: {:?}", ip_info);

        Box::new(esp_wifi)
    };

    log::info!("setting eventfd config");
    {
        esp_idf_sys::esp!(unsafe {
            esp_idf_sys::esp_vfs_eventfd_register(&esp_idf_sys::esp_vfs_eventfd_config_t {
                max_fds: 5,
                ..Default::default()
            })
        })?;
    }

    log::info!("starting executor");
    async_io::block_on(get_request())?;
    log::info!("stopped executor");

    loop {}
}
