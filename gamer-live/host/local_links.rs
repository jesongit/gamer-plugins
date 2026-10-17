//! Durable loopback read capabilities. Tokens never rotate on output/view toggles.
use anyhow::{ensure, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::Path;

static CREATION_GATE: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    stream_token: String,
    audience_token: String,
}

pub struct LocalLinks {
    saved: Saved,
    stream_url: String,
    audience_url: String,
}
#[derive(Debug, PartialEq)]
pub enum LocalTarget<'a> {
    Stream(&'a str),
    AudiencePage,
    AudienceState,
}
impl LocalLinks {
    pub fn open(path: &Path, port: u16) -> Result<Self> {
        let _gate = CREATION_GATE.lock();
        match std::fs::metadata(path) {
            Ok(metadata) => ensure!(
                metadata.is_file() && metadata.len() <= 1024,
                "直播本机地址配置文件无效或过大"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => anyhow::bail!("无法读取直播本机地址配置，未重置已保存地址"),
        }
        let saved = match std::fs::read(path) {
            Ok(bytes) => {
                ensure!(bytes.len() <= 1024, "直播本机地址配置文件过大");
                serde_json::from_slice::<Saved>(&bytes)
                    .map_err(|_| anyhow::anyhow!("直播本机地址配置文件损坏，未重置已保存地址"))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let saved = Saved {
                    stream_token: uuid::Uuid::new_v4().simple().to_string(),
                    audience_token: uuid::Uuid::new_v4().simple().to_string(),
                };
                let parent = path.parent().context("直播本机地址配置路径无效")?;
                std::fs::create_dir_all(parent)?;
                crate::core::fs::atomic_write(path, &serde_json::to_vec(&saved)?)
                    .context("直播本机地址保存失败")?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
                }
                saved
            }
            Err(_) => anyhow::bail!("无法读取直播本机地址配置，未重置已保存地址"),
        };
        ensure!(
            valid_token(&saved.stream_token)
                && valid_token(&saved.audience_token)
                && saved.stream_token != saved.audience_token,
            "直播本机地址配置无效，未重置已保存地址"
        );
        let origin = format!("http://127.0.0.1:{port}/local/extensions/gamer-live");
        let stream_url = format!("{origin}/{}/stream/index.m3u8", saved.stream_token);
        let audience_url = format!("{origin}/{}/audience/", saved.audience_token);
        Ok(Self {
            saved,
            stream_url,
            audience_url,
        })
    }
    pub fn stream_url(&self) -> &str {
        &self.stream_url
    }
    pub fn audience_url(&self) -> &str {
        &self.audience_url
    }
    pub fn resolve<'a>(&self, path: &'a str) -> Option<LocalTarget<'a>> {
        let (token, resource) = path.split_once('/')?;
        if token == self.saved.stream_token {
            let name = resource.strip_prefix("stream/")?;
            let segment = name.strip_suffix(".ts").is_some_and(|stem| {
                !stem.is_empty() && stem.len() <= 20 && stem.bytes().all(|b| b.is_ascii_digit())
            });
            return (name == "index.m3u8" || segment).then_some(LocalTarget::Stream(name));
        }
        if token == self.saved.audience_token {
            return match resource {
                "audience/" => Some(LocalTarget::AudiencePage),
                "audience/state" => Some(LocalTarget::AudienceState),
                _ => None,
            };
        }
        None
    }
}
fn valid_token(token: &str) -> bool {
    token.len() == 32
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && uuid::Uuid::parse_str(token).is_ok_and(|uuid| uuid.get_version_num() == 4)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_survive_recreation_and_do_not_rewrite_saved_capabilities() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("local-links.json");
        let links = LocalLinks::open(&path, 8443).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let restored = LocalLinks::open(&path, 8443).unwrap();
        assert_eq!(links.stream_url(), restored.stream_url());
        assert_eq!(links.audience_url(), restored.audience_url());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(links
            .stream_url()
            .starts_with("http://127.0.0.1:8443/local/extensions/gamer-live/"));
        assert!(links.stream_url().ends_with("/stream/index.m3u8"));
        assert!(links.audience_url().ends_with("/audience/"));
        assert_ne!(links.saved.stream_token, links.saved.audience_token);
        // A configured host port changes only the origin, never the capabilities.
        let other_port = LocalLinks::open(&path, 18548).unwrap();
        assert_eq!(other_port.saved.stream_token, links.saved.stream_token);
        assert_eq!(other_port.saved.audience_token, links.saved.audience_token);
    }
    #[test]
    fn dispatch_requires_exact_capability_kind_and_safe_filename() {
        let directory = tempfile::tempdir().unwrap();
        let links = LocalLinks::open(&directory.path().join("local-links.json"), 8443).unwrap();
        let stream = &links.saved.stream_token;
        let audience = &links.saved.audience_token;
        assert_eq!(
            links.resolve(&format!("{stream}/stream/index.m3u8")),
            Some(LocalTarget::Stream("index.m3u8"))
        );
        assert_eq!(
            links.resolve(&format!("{stream}/stream/00001.ts")),
            Some(LocalTarget::Stream("00001.ts"))
        );
        assert_eq!(
            links.resolve(&format!("{audience}/audience/")),
            Some(LocalTarget::AudiencePage)
        );
        assert_eq!(
            links.resolve(&format!("{audience}/audience/state")),
            Some(LocalTarget::AudienceState)
        );
        for path in [
            format!("{stream}/audience/state"),
            format!("{audience}/stream/index.m3u8"),
            format!("{audience}/audience"),
            format!("{audience}/audience/state/"),
            format!("{audience}/audience/api/runs"),
            format!("{stream}/stream/../index.m3u8"),
            format!("{stream}/stream/index.m3u8?query"),
            format!("{stream}/stream/%2e%2e"),
            format!("{stream}/stream/file.ts"),
            "invalid/audience/state".into(),
        ] {
            assert_eq!(links.resolve(&path), None, "unexpected resource accepted");
        }
    }
    #[test]
    fn malformed_saved_tokens_are_never_silently_rotated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("local-links.json");
        for bytes in [b"broken".as_slice(), b"{}", br#"{"stream_token":"bad","audience_token":"bad"}"#,
            br#"{"stream_token":"00000000000000000000000000000000","audience_token":"11111111111111111111111111111111"}"#] {
            std::fs::write(&path, bytes).unwrap();
            assert!(LocalLinks::open(&path, 8443).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        for value in [
            serde_json::json!({"stream_token":token,"audience_token":token}),
            serde_json::json!({"stream_token":token,"audience_token":uuid::Uuid::new_v4().simple().to_string(),"unknown":true}),
        ] {
            let bytes = serde_json::to_vec(&value).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(LocalLinks::open(&path, 8443).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }
}
