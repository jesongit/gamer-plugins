//! Local, optional OCR provider. The caller identity is captured by the host,
//! never accepted from script arguments.
use crate::extensions::{ExtensionService, PluginCallContext};
use anyhow::{anyhow, Result};
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::sync::{Arc, Weak};

pub type Recognizer = Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

pub fn recognizer(service: Weak<ExtensionService>, caller: PluginCallContext) -> Recognizer {
    Arc::new(move |values| {
        let service = service.clone();
        let caller = caller.clone();
        Box::pin(async move {
            let service = service
                .upgrade()
                .ok_or_else(|| anyhow!("OCR_UNAVAILABLE: OCR 本地识别服务不可用"))?;
            service
                .recognize_text(&caller, values)
                .await
                .map_err(|error| anyhow!("OCR_UNAVAILABLE: Core OCR 或本地引擎不可用: {error}"))
        })
    })
}
