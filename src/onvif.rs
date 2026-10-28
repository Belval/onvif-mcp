use anyhow::{Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use rand::RngCore;
use roxmltree::Document;
use serde::Serialize;
use sha1::{Digest, Sha1};
use std::{env, time::Duration};
use url::Url;

const DEVICE: &str = "http://www.onvif.org/ver10/device/wsdl";
const MEDIA: &str = "http://www.onvif.org/ver10/media/wsdl";
const PTZ: &str = "http://www.onvif.org/ver20/ptz/wsdl";

#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}
impl Config {
    pub fn load() -> Result<Self> {
        Ok(Self {
            host: env::var("CAMERA_HOST").unwrap_or_else(|_| "192.168.0.62".into()),
            port: env::var("CAMERA_PORT").unwrap_or_else(|_| "2020".into()).parse()?,
            username: env::var("CAMERA_USERNAME").map_err(|_| anyhow::anyhow!("Set CAMERA_USERNAME in the local .env"))?,
            password: env::var("CAMERA_PASSWORD").map_err(|_| anyhow::anyhow!("Set CAMERA_PASSWORD in the local .env"))?,
        })
    }
    pub fn validate_url(&self, input: &str, scheme: &str) -> Result<Url> {
        let url = Url::parse(input).map_err(|_| anyhow::anyhow!("Invalid camera endpoint"))?;
        ensure!(url.scheme() == scheme && url.host_str() == Some(self.host.as_str()), "Unexpected camera endpoint");
        ensure!(url.username().is_empty() && url.password().is_none(), "Unexpected endpoint credentials");
        Ok(url)
    }
}

#[derive(Clone, Serialize)]
pub struct Profile {
    pub token: String,
    pub name: String,
    pub encoding: String,
    pub width: u32,
    pub height: u32,
    pub ptz: bool,
}

pub struct Onvif {
    pub config: Config,
    client: reqwest::Client,
    device_url: String,
    media_url: String,
    ptz_url: String,
    clock_ms: i64,
    pub profiles: Vec<Profile>,
    pub profile: String,
    pub manufacturer: String,
    pub model: String,
    pub firmware: String,
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('"', "&quot;").replace('\'', "&apos;")
}
fn text(xml: &str, name: &str) -> Result<String> {
    let doc = Document::parse(xml)?;
    Ok(doc.descendants().find(|n| n.has_tag_name(name)).and_then(|n| n.text()).unwrap_or("").to_owned())
}
fn child_text(node: roxmltree::Node<'_, '_>, name: &str) -> String {
    node.descendants().find(|n| n.has_tag_name(name)).and_then(|n| n.text()).unwrap_or("").to_owned()
}

impl Onvif {
    pub async fn connect(config: Config) -> Result<Self> {
        let endpoint = format!("http://{}:{}/onvif/device_service", config.host, config.port);
        config.validate_url(&endpoint, "http")?;
        let mut camera = Self {
            config, client: reqwest::Client::builder().no_proxy().connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(5)).pool_idle_timeout(Duration::from_secs(90)).build()?,
            device_url: endpoint.clone(), media_url: endpoint.clone(), ptz_url: endpoint,
            clock_ms: 0, profiles: vec![], profile: String::new(),
            manufacturer: String::new(), model: String::new(), firmware: String::new(),
        };
        let date = camera.call(&camera.device_url, DEVICE, "GetSystemDateAndTime", "").await?;
        let doc = Document::parse(&date)?;
        if let Some(utc) = doc.descendants().find(|n| n.has_tag_name("UTCDateTime")) {
            let number = |name: &str| child_text(utc, name).parse::<u32>().unwrap_or(0);
            if let Some(date) = Utc.with_ymd_and_hms(number("Year") as i32, number("Month"), number("Day"),
                number("Hour"), number("Minute"), number("Second")).single() {
                camera.clock_ms = date.timestamp_millis() - Utc::now().timestamp_millis();
            }
        }
        let caps = camera.call(&camera.device_url, DEVICE, "GetCapabilities", "<s:Category>All</s:Category>").await?;
        let doc = Document::parse(&caps)?;
        for (name, dest) in [("Media", &mut camera.media_url), ("PTZ", &mut camera.ptz_url)] {
            let node = doc.descendants().find(|n| n.has_tag_name(name)).ok_or_else(|| anyhow::anyhow!("Camera service unavailable"))?;
            let endpoint = child_text(node, "XAddr");
            camera.config.validate_url(&endpoint, "http")?;
            *dest = endpoint;
        }
        let info = camera.call(&camera.device_url, DEVICE, "GetDeviceInformation", "").await?;
        camera.manufacturer = text(&info, "Manufacturer")?;
        camera.model = text(&info, "Model")?;
        camera.firmware = text(&info, "FirmwareVersion")?;
        let profiles = camera.call(&camera.media_url, MEDIA, "GetProfiles", "").await?;
        let doc = Document::parse(&profiles)?;
        for node in doc.descendants().filter(|n| n.has_tag_name("Profiles")) {
            let video = node.descendants().find(|n| n.has_tag_name("VideoEncoderConfiguration"));
            camera.profiles.push(Profile {
                token: node.attribute("token").unwrap_or("").to_owned(),
                name: child_text(node, "Name"),
                encoding: video.map(|n| child_text(n, "Encoding")).unwrap_or_default(),
                width: video.and_then(|n| child_text(n, "Width").parse().ok()).unwrap_or(0),
                height: video.and_then(|n| child_text(n, "Height").parse().ok()).unwrap_or(0),
                ptz: node.descendants().any(|n| n.has_tag_name("PTZConfiguration")),
            });
        }
        camera.profile = camera.profiles.iter().find(|p| p.ptz).ok_or_else(|| anyhow::anyhow!("No pan/tilt profile"))?.token.clone();
        Ok(camera)
    }

    fn security(&self, created: DateTime<Utc>, nonce: &[u8]) -> String {
        let created = created.to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut digest = Sha1::new();
        digest.update(nonce); digest.update(created.as_bytes()); digest.update(self.config.password.as_bytes());
        format!(r#"<wsse:Security xmlns:wsse="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd" xmlns:wsu="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd"><wsse:UsernameToken><wsse:Username>{}</wsse:Username><wsse:Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">{}</wsse:Password><wsse:Nonce EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary">{}</wsse:Nonce><wsu:Created>{}</wsu:Created></wsse:UsernameToken></wsse:Security>"#,
            escape(&self.config.username), STANDARD.encode(digest.finalize()), STANDARD.encode(nonce), created)
    }

    async fn call(&self, endpoint: &str, namespace: &str, action: &str, body: &str) -> Result<String> {
        let mut nonce = [0u8; 20]; rand::rng().fill_bytes(&mut nonce);
        let security = self.security(Utc::now() + chrono::Duration::milliseconds(self.clock_ms), &nonce);
        let xml = format!(r#"<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope" xmlns:s="{namespace}" xmlns:tt="http://www.onvif.org/ver10/schema"><env:Header>{security}</env:Header><env:Body><s:{action}>{body}</s:{action}></env:Body></env:Envelope>"#);
        let response = self.client.post(endpoint).header("Content-Type", format!("application/soap+xml; charset=utf-8; action=\"{namespace}/{action}\""))
            .body(xml).send().await.map_err(|_| anyhow::anyhow!("ONVIF {action} connection failed"))?;
        let success = response.status().is_success();
        let result = response.text().await.map_err(|_| anyhow::anyhow!("ONVIF response unavailable"))?;
        let doc = Document::parse(&result).map_err(|_| anyhow::anyhow!("Invalid ONVIF response"))?;
        if !success || doc.descendants().any(|n| n.has_tag_name("Fault")) {
            bail!("ONVIF {action} rejected; check camera account and connectivity");
        }
        Ok(result)
    }
    pub async fn stream_url(&self, token: &str) -> Result<Url> {
        let body = format!("<s:StreamSetup><tt:Stream>RTP-Unicast</tt:Stream><tt:Transport><tt:Protocol>RTSP</tt:Protocol></tt:Transport></s:StreamSetup><s:ProfileToken>{}</s:ProfileToken>", escape(token));
        let xml = self.call(&self.media_url, MEDIA, "GetStreamUri", &body).await?;
        self.config.validate_url(&text(&xml, "Uri")?, "rtsp")
    }
    pub async fn position(&self) -> Result<[f64; 2]> {
        let xml = self.call(&self.ptz_url, PTZ, "GetStatus", &format!("<s:ProfileToken>{}</s:ProfileToken>", escape(&self.profile))).await?;
        let doc = Document::parse(&xml)?;
        let pan = doc.descendants().find(|n| n.has_tag_name("Position")).and_then(|p| p.descendants().find(|n| n.has_tag_name("PanTilt")))
            .ok_or_else(|| anyhow::anyhow!("Camera position unavailable"))?;
        Ok([pan.attribute("x").unwrap_or("").parse()?, pan.attribute("y").unwrap_or("").parse()?])
    }
    pub async fn stop(&self) -> Result<()> {
        self.call(&self.ptz_url, PTZ, "Stop", &format!("<s:ProfileToken>{}</s:ProfileToken><s:PanTilt>true</s:PanTilt><s:Zoom>true</s:Zoom>", escape(&self.profile))).await?;
        Ok(())
    }
    pub async fn absolute(&self, target: [f64; 2]) -> Result<()> {
        ensure!(target.iter().all(|v| v.is_finite() && (-1.0..=1.0).contains(v)), "Invalid camera position");
        self.call(&self.ptz_url, PTZ, "AbsoluteMove", &format!("<s:ProfileToken>{}</s:ProfileToken><s:Position><tt:PanTilt x=\"{}\" y=\"{}\"/></s:Position>", escape(&self.profile), target[0], target[1])).await?;
        Ok(())
    }
    pub async fn velocity(&self, pan: f64, tilt: f64, seconds: f64) -> Result<()> {
        ensure!([pan, tilt, seconds].iter().all(|v| v.is_finite()) && pan.abs() <= 1.0 && tilt.abs() <= 1.0 && seconds > 0.0 && seconds <= 2.0, "Invalid movement values");
        self.call(&self.ptz_url, PTZ, "ContinuousMove", &format!("<s:ProfileToken>{}</s:ProfileToken><s:Velocity><tt:PanTilt x=\"{pan}\" y=\"{tilt}\"/></s:Velocity><s:Timeout>PT{}S</s:Timeout>", escape(&self.profile), seconds.ceil())).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn escape_xml() { assert_eq!(escape("a&<\"'"), "a&amp;&lt;&quot;&apos;"); }
    #[test] fn reject_other_hosts_and_credentials() {
        let c = Config { host: "192.168.0.62".into(), port: 2020, username: "user".into(), password: "secret".into() };
        assert!(c.validate_url("rtsp://192.168.0.62/stream3", "rtsp").is_ok());
        assert!(c.validate_url("rtsp://other/stream3", "rtsp").is_err());
        assert!(c.validate_url("rtsp://user:secret@192.168.0.62/stream3", "rtsp").is_err());
    }
}
