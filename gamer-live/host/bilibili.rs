//! Official Bilibili APIs only. OAuth and OpenLive credentials never cross modes.
use super::{events::EventBuffer, ConnectionStatus};
use anyhow::{bail, ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use std::{io::Read, sync::Arc, time::Duration};
use tokio::sync::watch;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{protocol::WebSocketConfig, Message},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    pub mode: String,
    pub access_key: String,
    pub access_secret: String,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub identity_code: String,
    #[serde(default)]
    pub access_token: String,
}
impl Credentials {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.mode.as_str(), "open_live" | "oauth"),
            "请选择 B站接入方式"
        );
        for value in [&self.access_key, &self.access_secret] {
            ensure!(
                !value.trim().is_empty() && value.len() <= 4096,
                "开发者凭据不能为空或过长"
            );
        }
        ensure!(
            self.access_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "Access Key ID 格式无效"
        );
        if self.mode == "open_live" {
            ensure!(
                self.app_id.parse::<u64>().is_ok()
                    && !self.identity_code.trim().is_empty()
                    && self.identity_code.len() <= 256,
                "请填写有效的应用 ID 和主播身份码"
            );
            ensure!(
                self.access_token.is_empty(),
                "身份码模式不能混用 OAuth Token"
            );
        } else {
            ensure!(
                !self.access_token.is_empty() && self.access_token.len() <= 8192,
                "请填写有效的 OAuth Access Token"
            );
            ensure!(
                self.identity_code.is_empty() && self.app_id.is_empty(),
                "OAuth 模式不能混用主播身份码"
            );
        }
        Ok(())
    }
}

fn signed_headers(
    c: &Credentials,
    body: &str,
    timestamp: &str,
    nonce: &str,
) -> Result<reqwest::header::HeaderMap> {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    let values = [
        ("x-bili-accesskeyid", c.access_key.clone()),
        ("x-bili-content-md5", format!("{:x}", Md5::digest(body))),
        ("x-bili-signature-method", "HMAC-SHA256".into()),
        ("x-bili-signature-nonce", nonce.into()),
        (
            "x-bili-signature-version",
            if c.mode == "oauth" { "2.0" } else { "1.0" }.into(),
        ),
        ("x-bili-timestamp", timestamp.into()),
    ];
    let canonical = values
        .iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut mac = Hmac::<Sha256>::new_from_slice(c.access_secret.as_bytes())?;
    mac.update(canonical.as_bytes());
    let signature = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut h = HeaderMap::new();
    for (k, v) in values {
        h.insert(
            HeaderName::from_bytes(k.as_bytes())?,
            HeaderValue::from_str(&v).context("凭据包含非法字符")?,
        );
    }
    h.insert("authorization", HeaderValue::from_str(&signature)?);
    h.insert("content-type", HeaderValue::from_static("application/json"));
    h.insert("accept", HeaderValue::from_static("application/json"));
    if c.mode == "oauth" {
        h.insert(
            "access-token",
            HeaderValue::from_str(&c.access_token).context("Token 包含非法字符")?,
        );
    }
    Ok(h)
}
struct Client {
    http: reqwest::Client,
    credentials: Credentials,
    base: String,
}
impl Client {
    fn new(credentials: Credentials) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base: if credentials.mode == "oauth" {
                "https://member.bilibili.com"
            } else {
                "https://live-open.biliapi.com"
            }
            .into(),
            credentials,
        })
    }
    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let body = if body.is_null() {
            String::new()
        } else {
            serde_json::to_string(&body)?
        };
        let headers = signed_headers(
            &self.credentials,
            &body,
            &chrono::Utc::now().timestamp().to_string(),
            &uuid::Uuid::new_v4().simple().to_string(),
        )?;
        let base = &self.base;
        let response = self
            .http
            .post(format!("{base}{path}"))
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("B站接口连接失败，请检查网络"))?;
        ensure!(
            response.status().is_success(),
            "B站 HTTP 错误 {}",
            response.status().as_u16()
        );
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("B站响应读取失败"))?
        {
            ensure!(bytes.len() + chunk.len() <= 1024 * 1024, "B站响应过大");
            bytes.extend_from_slice(&chunk);
        }
        let response: Value = serde_json::from_slice(&bytes).context("B站返回了无效响应")?;
        let code = response["code"].as_i64().context("B站响应缺少状态码")?;
        // Do not return remote error text: some gateways echo request credentials.
        ensure!(
            code == 0,
            "B站接口返回错误码 {code}，请核对接入方式、授权和应用权限"
        );
        Ok(response["data"].clone())
    }
    async fn begin(&self) -> Result<(String, String, String, Option<Value>)> {
        let c = &self.credentials;
        let d = if c.mode == "oauth" {
            self.post("/arcopen/fn/live/room/ws-start", Value::Null)
                .await?
        } else {
            self.post(
                "/v2/app/start",
                json!({"code":c.identity_code,"app_id":c.app_id.parse::<u64>()?}),
            )
            .await?
        };
        let id = if c.mode == "oauth" {
            d["conn_id"].as_str()
        } else {
            d["game_info"]["game_id"].as_str()
        }
        .context("平台未返回连接 ID")?
        .to_owned();
        let ws = d["websocket_info"]["wss_link"][0]
            .as_str()
            .context("平台未返回长连地址")?
            .to_owned();
        let url = reqwest::Url::parse(&ws)?;
        ensure!(
            url.scheme() == "wss"
                && url.host_str().is_some_and(|h| h.ends_with(".bilibili.com")
                    || h.ends_with(".biliapi.com")
                    || h.ends_with(".bilivideo.com")),
            "平台返回的长连地址不受信任"
        );
        let auth = d["websocket_info"]["auth_body"]
            .as_str()
            .context("平台未返回长连授权")?
            .to_owned();
        Ok((id, ws, auth, d.get("anchor_info").cloned()))
    }
    async fn heartbeat(&self, id: &str) -> Result<()> {
        if self.credentials.mode == "oauth" {
            self.post(
                &format!(
                    "/arcopen/fn/live/room/ws-heartbeat?client_id={}",
                    self.credentials.access_key
                ),
                json!({"conn_id":id}),
            )
            .await?;
        } else {
            self.post("/v2/app/heartbeat", json!({"game_id":id}))
                .await?;
        }
        Ok(())
    }
    async fn end(&self, id: &str) {
        if self.credentials.mode == "open_live" {
            let _=self.post("/v2/app/end",json!({"game_id":id,"app_id":self.credentials.app_id.parse::<u64>().unwrap_or_default()})).await;
        }
    }
}
pub fn packet(operation: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 16);
    out.extend_from_slice(&((body.len() + 16) as u32).to_be_bytes());
    out.extend_from_slice(&16u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&operation.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(body);
    out
}
pub fn decode(data: &[u8]) -> Result<Vec<(u32, Value)>> {
    fn parse(
        mut data: &[u8],
        depth: usize,
        budget: &mut usize,
        bytes_left: &mut usize,
        out: &mut Vec<(u32, Value)>,
    ) -> Result<()> {
        ensure!(depth <= 4, "长连压缩层级超限");
        while !data.is_empty() {
            ensure!(data.len() >= 16, "长连数据头不完整");
            let len = u32::from_be_bytes(data[..4].try_into()?) as usize;
            let header = u16::from_be_bytes(data[4..6].try_into()?) as usize;
            let version = u16::from_be_bytes(data[6..8].try_into()?);
            let op = u32::from_be_bytes(data[8..12].try_into()?);
            ensure!(
                header >= 16 && len >= header && len <= data.len() && len <= 4 * 1024 * 1024,
                "长连数据长度无效"
            );
            ensure!(*budget > 0, "长连消息数量超限");
            *budget -= 1;
            let body = &data[header..len];
            if version == 2 {
                let mut expanded = Vec::new();
                flate2::read::ZlibDecoder::new(body)
                    .take(4 * 1024 * 1024 + 1)
                    .read_to_end(&mut expanded)?;
                ensure!(expanded.len() <= *bytes_left, "长连解压大小超限");
                *bytes_left -= expanded.len();
                parse(&expanded, depth + 1, budget, bytes_left, out)?;
            } else if op == 5 || op == 8 {
                ensure!(version <= 1, "不支持的长连协议版本");
                out.push((op, serde_json::from_slice(body).context("长连 JSON 无效")?));
            }
            data = &data[len..];
        }
        Ok(())
    }
    let mut out = Vec::new();
    ensure!(data.len() <= 4 * 1024 * 1024, "长连消息过大");
    parse(data, 0, &mut 4096, &mut (8 * 1024 * 1024), &mut out)?;
    Ok(out)
}

pub async fn run(
    credentials: Credentials,
    status: Arc<Mutex<ConnectionStatus>>,
    events: Arc<Mutex<EventBuffer>>,
    mut stop: watch::Receiver<bool>,
) {
    let client = match Client::new(credentials) {
        Ok(c) => c,
        Err(_) => {
            status.lock().state = "failed".into();
            return;
        }
    };
    let mut retry = 0u32;
    loop {
        if *stop.borrow() {
            break;
        }
        status.lock().state = if retry == 0 {
            "connecting"
        } else {
            "reconnecting"
        }
        .into();
        let started = tokio::select! {_=stop.changed()=>break,result=client.begin()=>result};
        let result = match started {
            Ok((id, ws, auth, anchor)) => {
                {
                    let mut s = status.lock();
                    s.room = anchor;
                    s.connection_id = id.clone();
                }
                let result = tokio::select! {_=stop.changed()=>Ok(()),result=listen(&client,&id,&ws,&auth,&status,&events)=>result};
                client.end(&id).await;
                result
            }
            Err(e) => Err(e),
        };
        if *stop.borrow() {
            break;
        }
        if let Err(error) = result {
            status.lock().error = Some(error.to_string());
        }
        retry += 1;
        status.lock().reconnects = retry;
        if retry >= 6 {
            status.lock().state = "failed".into();
            return;
        }
        tokio::select! {_=stop.changed()=>break,_=tokio::time::sleep(Duration::from_secs(1<<retry.min(4)))=>{}}
    }
    status.lock().state = "disconnected".into();
}
async fn listen(
    client: &Client,
    id: &str,
    url: &str,
    auth: &str,
    status: &Arc<Mutex<ConnectionStatus>>,
    events: &Arc<Mutex<EventBuffer>>,
) -> Result<()> {
    let config = WebSocketConfig {
        max_message_size: Some(4 * 1024 * 1024),
        max_frame_size: Some(4 * 1024 * 1024),
        ..Default::default()
    };
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        connect_async_with_config(url, Some(config), false),
    )
    .await
    .context("长连超时")?
    .map_err(|_| anyhow::anyhow!("长连建立失败"))?;
    socket
        .send(Message::Binary(packet(7, auth.as_bytes())))
        .await
        .map_err(|_| anyhow::anyhow!("长连授权发送失败"))?;
    let mut interval = tokio::time::interval(Duration::from_secs(20));
    let mut last = tokio::time::Instant::now();
    let mut authorized = false;
    let auth_deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(auth_deadline);
    loop {
        tokio::select! {
            _=&mut auth_deadline, if !authorized => bail!("长连授权确认超时"),
            _=interval.tick()=>{
                ensure!(last.elapsed()<Duration::from_secs(60),"长连心跳超时");
                client.heartbeat(id).await?;
                socket.send(Message::Binary(packet(2,b""))).await.map_err(|_|anyhow::anyhow!("长连心跳发送失败"))?;
            },
            message=socket.next()=>{
                let message=message.context("平台关闭了长连")?.map_err(|_|anyhow::anyhow!("长连接收失败"))?;
                last=tokio::time::Instant::now();
                if let Message::Close(_)=message {bail!("平台关闭了长连");}
                if let Message::Ping(body)=message {socket.send(Message::Pong(body)).await?;continue;}
                if let Message::Binary(bytes)=message {
                    for (op,event) in decode(&bytes)? {
                        if op==8 {ensure!(event["code"].as_i64()==Some(0),"平台拒绝了长连授权");authorized=true;let mut s=status.lock();s.state="connected".into();s.error=None;}
                        else if authorized {
                            let ended=event["cmd"].as_str().is_some_and(|c|c.ends_with("INTERACTION_END"));
                            events.lock().push(id,event);
                            if ended {bail!("平台消息推送已结束，需要重新建立连接");}
                        }
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packet_bounds_and_batched_messages() {
        let a = packet(5, br#"{"cmd":"OPEN_LIVEROOM_DM","data":{}}"#);
        let mut b = a.clone();
        b.extend(&a);
        assert_eq!(decode(&b).unwrap().len(), 2);
        assert!(decode(&a[..15]).is_err());
        let mut bad = a;
        bad[..4].copy_from_slice(&1u32.to_be_bytes());
        assert!(decode(&bad).is_err());
    }
    #[test]
    fn modes_cannot_mix() {
        let c = Credentials {
            mode: "oauth".into(),
            access_key: "x".into(),
            access_secret: "y".into(),
            access_token: "z".into(),
            identity_code: "no".into(),
            app_id: String::new(),
        };
        assert!(c.validate().is_err());
    }
    #[test]
    fn signature_is_stable_and_body_sensitive() {
        let c = Credentials {
            mode: "open_live".into(),
            access_key: "key".into(),
            access_secret: "secret".into(),
            app_id: "1".into(),
            identity_code: "code".into(),
            access_token: String::new(),
        };
        let a = signed_headers(&c, "{}", "1", "2").unwrap();
        let b = signed_headers(&c, "[]", "1", "2").unwrap();
        assert_ne!(a["authorization"], b["authorization"]);
        assert_eq!(a["x-bili-content-md5"], "99914b932bd37a50b983c5e7c90ae93b");
    }
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod protocol_tests;
