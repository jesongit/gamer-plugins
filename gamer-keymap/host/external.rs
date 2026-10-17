//! Explicit external profile tools. Editing a resource never hot-switches the
//! guest instance or bypasses the input-control admission gate.
use crate::{
    extensions::ExternalToolHandler,
    mcp::{ErrorKind, InvocationContext, McpError, ToolResult},
    resources::{PackageStore, SaveValidation},
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) struct ExternalTools {
    packages: Arc<PackageStore>,
}
impl ExternalTools {
    pub(crate) fn new(packages: Arc<PackageStore>) -> Self {
        Self { packages }
    }

    fn persist_profile(
        &self,
        context: &InvocationContext,
        request: &Write,
        path: &str,
    ) -> Result<crate::resources::ResourceEntry, McpError> {
        self.packages
            .with_package_write(&request.package_id, || {
                // A Package replacement or another resource transaction can
                // hold this barrier beyond credential revocation.
                context.check_active().map_err(anyhow::Error::new)?;
                self.packages.write_text(
                    &request.package_id,
                    super::KEYMAP_EXTENSION_ID,
                    path,
                    &request.content,
                    request.expected_version.as_deref(),
                    false,
                )
            })
            .map_err(business)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    package_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    package_id: String,
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Validate {
    package_id: String,
    name: String,
    content: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
    package_id: String,
    name: String,
    content: String,
    expected_version: Option<String>,
    operation_id: String,
}

fn profile_path(name: &str) -> Result<String, McpError> {
    if name.is_empty()
        || name.len() > 160
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
        || !(name.ends_with(".yaml") || name.ends_with(".yml"))
    {
        return Err(McpError::invalid(
            "Profile name must be a YAML short filename",
        ));
    }
    Ok(format!("{}{name}", super::KEYMAP_PROFILE_PREFIX))
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, McpError> {
    serde_json::from_value(value).map_err(|_| McpError::invalid("Invalid keymap tool arguments"))
}
fn business(error: impl std::fmt::Display) -> McpError {
    let message = error.to_string();
    if message.starts_with("version_") {
        McpError::new(ErrorKind::Conflict, "version_conflict", message)
    } else {
        McpError::invalid(message)
    }
}

#[async_trait]
impl ExternalToolHandler for ExternalTools {
    fn trusted_manifest(&self) -> Option<&'static str> {
        Some(include_str!("../manifest.toml"))
    }
    async fn authorize(
        &self,
        context: &InvocationContext,
        action: &str,
        arguments: &Value,
    ) -> Result<(), McpError> {
        context.check_active()?;
        context.authorize_plugin(super::KEYMAP_EXTENSION_ID)?;
        let scope = match action {
            "profiles.list" | "profiles.read" | "profiles.validate" => {
                "plugin.gamer-keymap.profiles.read"
            }
            "profiles.write" => "plugin.gamer-keymap.profiles.write",
            _ => {
                return Err(McpError::forbidden(
                    "Keymap action is not exposed externally",
                ))
            }
        };
        context.require_scope(scope)?;
        context.require_scope(if action == "profiles.write" {
            "resource.write"
        } else {
            "resource.read"
        })?;
        let package = arguments["package_id"]
            .as_str()
            .ok_or_else(|| McpError::invalid("package_id required"))?;
        context.authorize_package(package)?;
        self.packages.manifest(package).map_err(business)?;
        if action != "profiles.list" {
            profile_path(
                arguments["name"]
                    .as_str()
                    .ok_or_else(|| McpError::invalid("name required"))?,
            )?;
        }
        Ok(())
    }

    async fn call(
        &self,
        context: InvocationContext,
        action: &str,
        arguments: Value,
    ) -> Result<ToolResult, McpError> {
        self.authorize(&context, action, &arguments).await?;
        match action {
            "profiles.list" => {
                let request: Package = decode(arguments)?;
                let entries = self
                    .packages
                    .list(
                        &request.package_id,
                        super::KEYMAP_EXTENSION_ID,
                        super::KEYMAP_PROFILE_PREFIX,
                    )
                    .map_err(business)?;
                Ok(ToolResult::json(
                    json!({"package_id":request.package_id,"profiles":entries}),
                ))
            }
            "profiles.read" => {
                let request: Profile = decode(arguments)?;
                let path = profile_path(&request.name)?;
                let entry = self
                    .packages
                    .read_text(&request.package_id, super::KEYMAP_EXTENSION_ID, &path)
                    .map_err(business)?
                    .ok_or_else(|| {
                        McpError::new(
                            ErrorKind::NotFound,
                            "profile_not_found",
                            "Keymap profile does not exist",
                        )
                    })?;
                Ok(ToolResult::json(
                    serde_json::to_value(entry).map_err(business)?,
                ))
            }
            "profiles.validate" => {
                let request: Validate = decode(arguments)?;
                let path = profile_path(&request.name)?;
                if request.content.len() > super::MAX_KEYMAP_YAML_BYTES {
                    return Err(McpError::invalid("Keymap source is too large"));
                }
                let result = self.packages.validate_save(SaveValidation {
                    package: &request.package_id,
                    plugin: super::KEYMAP_EXTENSION_ID,
                    path: &path,
                    content: &request.content,
                    store: &self.packages,
                });
                let direct = super::parse_keymap_content(&request.content, &path);
                let diagnostics = match (result, direct) {
                    (Err(diagnostics), _) => diagnostics,
                    (_, Err(diagnostics)) => serde_json::to_value(diagnostics).map_err(business)?,
                    _ => json!([]),
                };
                Ok(ToolResult::json(
                    json!({"valid":diagnostics.as_array().is_some_and(Vec::is_empty),"diagnostics":diagnostics,"scope":"keymap_syntax","activated":false}),
                ))
            }
            "profiles.write" => {
                let request: Write = decode(arguments)?;
                if context.operation_id.as_deref() != Some(request.operation_id.as_str()) {
                    return Err(McpError::invalid("operation_id mismatch"));
                }
                let path = profile_path(&request.name)?;
                if request.content.len() > super::MAX_KEYMAP_YAML_BYTES {
                    return Err(McpError::invalid("Keymap source is too large"));
                }
                super::parse_keymap_content(&request.content, &path).map_err(|diagnostics| {
                    McpError::invalid(serde_json::to_string(&diagnostics).unwrap_or_default())
                })?;
                self.packages
                    .validate_save(SaveValidation {
                        package: &request.package_id,
                        plugin: super::KEYMAP_EXTENSION_ID,
                        path: &path,
                        content: &request.content,
                        store: &self.packages,
                    })
                    .map_err(|diagnostics| McpError::invalid(diagnostics.to_string()))?;
                let entry = self.persist_profile(&context, &request, &path)?;
                Ok(ToolResult::json(
                    json!({"resource":entry,"activated":false,"note":"Select the profile in Gamer to activate it"}),
                ))
            }
            _ => Err(McpError::forbidden(
                "Keymap action is not exposed externally",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn revoked_profile_write_waiting_for_the_resource_barrier_preserves_the_file() {
        let root = tempfile::tempdir().unwrap();
        let packages = Arc::new(
            PackageStore::open(&crate::config::Config {
                data_dir: root.path().into(),
                ..Default::default()
            })
            .unwrap(),
        );
        packages.ensure_default_package().unwrap();
        let original = packages
            .write_text(
                "default",
                super::super::KEYMAP_EXTENSION_ID,
                "mappings/test.yaml",
                "old",
                None,
                false,
            )
            .unwrap();
        let mcp = crate::mcp::McpService::open(
            root.path(),
            crate::mcp::ServerInfo {
                name: "test".into(),
                version: "1".into(),
            },
        )
        .unwrap();
        let credentials = mcp.credentials();
        let token = credentials
            .create(crate::mcp::CreateTokenRequest {
                label: "test".into(),
                scopes: [
                    "resource.write".into(),
                    "plugin.gamer-keymap.profiles.write".into(),
                ]
                .into(),
                allowed: crate::mcp::AllowedObjects {
                    packages: ["default".into()].into(),
                    plugins: [super::super::KEYMAP_EXTENSION_ID.into()].into(),
                    ..Default::default()
                },
                expires_in_seconds: 60,
            })
            .unwrap();
        let context = InvocationContext::new(
            credentials
                .principal_for_id(&token.metadata.token_id)
                .unwrap(),
            "test".into(),
            Some("write".into()),
            credentials.clone(),
            mcp.receipts(),
        );
        let (locked, acquired) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let holder = std::thread::spawn({
            let packages = packages.clone();
            move || {
                packages
                    .with_package_write("default", || {
                        locked.send(()).unwrap();
                        released
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .unwrap();
                        Ok(())
                    })
                    .unwrap()
            }
        });
        acquired
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pending = tokio::task::spawn_blocking({
            let tools = ExternalTools::new(packages.clone());
            let started = started.clone();
            move || {
                started.store(true, std::sync::atomic::Ordering::Release);
                tools.persist_profile(
                    &context,
                    &Write {
                        package_id: "default".into(),
                        name: "test.yaml".into(),
                        content: "new".into(),
                        expected_version: Some(original.version()),
                        operation_id: "write".into(),
                    },
                    "mappings/test.yaml",
                )
            }
        });
        while !started.load(std::sync::atomic::Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!pending.is_finished());
        credentials.revoke(&token.metadata.token_id).unwrap();
        release.send(()).unwrap();
        holder.join().unwrap();
        assert!(pending.await.unwrap().is_err());
        assert_eq!(
            packages
                .read_text(
                    "default",
                    super::super::KEYMAP_EXTENSION_ID,
                    "mappings/test.yaml"
                )
                .unwrap()
                .unwrap()
                .content,
            "old"
        );
    }
    #[tokio::test]
    async fn a_read_classified_context_cannot_write_using_a_client_operation_id() {
        let root = tempfile::tempdir().unwrap();
        let packages = Arc::new(
            PackageStore::open(&crate::config::Config {
                data_dir: root.path().into(),
                ..Default::default()
            })
            .unwrap(),
        );
        packages.ensure_default_package().unwrap();
        let mcp = crate::mcp::McpService::open(
            root.path(),
            crate::mcp::ServerInfo {
                name: "test".into(),
                version: "1".into(),
            },
        )
        .unwrap();
        let credentials = mcp.credentials();
        let created = credentials
            .create(crate::mcp::CreateTokenRequest {
                label: "test".into(),
                scopes: [
                    "resource.write".into(),
                    "plugin.gamer-keymap.profiles.write".into(),
                ]
                .into(),
                allowed: crate::mcp::AllowedObjects {
                    packages: ["default".into()].into(),
                    plugins: [super::super::KEYMAP_EXTENSION_ID.into()].into(),
                    ..Default::default()
                },
                expires_in_seconds: 300,
            })
            .unwrap();
        let context = InvocationContext::new(
            credentials
                .principal_for_id(&created.metadata.token_id)
                .unwrap(),
            "forged_read_tool".into(),
            None,
            credentials,
            mcp.receipts(),
        );
        let tools = ExternalTools::new(packages.clone());
        assert!(tools.call(context,"profiles.write",json!({"package_id":"default","name":"forged.yaml","content":"","operation_id":"client-only"})).await.is_err());
        assert!(packages
            .read_text(
                "default",
                super::super::KEYMAP_EXTENSION_ID,
                "mappings/forged.yaml"
            )
            .unwrap()
            .is_none());
    }
    #[test]
    fn profiles_are_short_names_and_cannot_select_another_plugin_or_activate() {
        assert!(profile_path("default.yaml").is_ok());
        for name in [
            "../settings.yaml",
            "a/b.yaml",
            "a\\b.yaml",
            "profile.json",
            "",
        ] {
            assert!(profile_path(name).is_err());
        }
        assert!(serde_json::from_value::<Write>(json!({"package_id":"p","name":"a.yaml","content":"","expected_version":null,"operation_id":"one","activate":true})).is_err());
    }
}
