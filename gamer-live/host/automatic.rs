//! One live workflow owns one output and reuses the interaction FIFO. Waiting
//! for viewers and playing files never creates a Core automation run.
use super::queue::{DispatchHooks, Queue, Target};
use crate::{
    device::DeviceManager,
    media::output::{self, OutputHandle, OutputRequest, OutputStatus},
};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Weak,
};
use tokio::sync::Mutex as AsyncMutex;

pub type OutputSlot = Arc<AsyncMutex<Option<OutputHandle>>>;

#[async_trait]
trait Media: Send + Sync {
    async fn start(&self, request: OutputRequest) -> Result<()>;
    async fn device(&self, device: &str) -> Result<()>;
    async fn video(&self) -> Result<()>;
    async fn stop(&self);
    async fn status(&self) -> Option<Value>;
    async fn reliable_status(&self) -> Option<Value> {
        self.status().await
    }
}
struct CoreMedia {
    devices: Arc<DeviceManager>,
    output: OutputSlot,
    cached: Mutex<Option<Arc<Mutex<OutputStatus>>>>,
    local_source_url: String,
}
#[async_trait]
impl Media for CoreMedia {
    async fn start(&self, request: OutputRequest) -> Result<()> {
        let mut slot = self.output.lock().await;
        ensure!(
            slot.as_ref().is_none_or(|h| h.finished()),
            "已有音视频输出，请先停止"
        );
        if let Some(old) = slot.take() {
            old.stop().await;
        }
        let source_url = (request.mode == "local").then(|| self.local_source_url.clone());
        let handle = output::start(self.devices.clone(), request, source_url).await?;
        *self.cached.lock() = Some(handle.status.clone());
        *slot = Some(handle);
        Ok(())
    }
    async fn device(&self, device: &str) -> Result<()> {
        self.output
            .lock()
            .await
            .as_ref()
            .context("自动直播输出已结束")?
            .switch_device(device)
            .await?;
        Ok(())
    }
    async fn video(&self) -> Result<()> {
        self.output
            .lock()
            .await
            .as_ref()
            .context("自动直播输出已结束")?
            .switch_video_folder()
            .await?;
        Ok(())
    }
    async fn stop(&self) {
        if let Some(handle) = self.output.lock().await.take() {
            handle.stop().await;
        }
        self.cached.lock().take();
    }
    async fn status(&self) -> Option<Value> {
        if let Ok(output) = self.output.try_lock() {
            *self.cached.lock() = output.as_ref().map(|h| h.status.clone());
        }
        self.cached.lock().as_ref().map(|s| json!(s.lock().clone()))
    }
    async fn reliable_status(&self) -> Option<Value> {
        let output = self.output.lock().await;
        let status = output.as_ref().map(|h| h.status.clone());
        *self.cached.lock() = status.clone();
        status.map(|s| json!(s.lock().clone()))
    }
}
#[derive(Clone, Serialize)]
struct Status {
    state: String,
    video_folder: String,
    device_id: String,
    package_id: String,
    error: Option<String>,
    #[serde(skip)]
    owns_output: bool,
    #[serde(skip)]
    ready: bool,
    #[serde(skip)]
    video_source: bool,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            state: "stopped".into(),
            video_folder: String::new(),
            device_id: String::new(),
            package_id: String::new(),
            error: None,
            owns_output: false,
            ready: false,
            video_source: true,
        }
    }
}
pub struct Automatic {
    state: Mutex<Status>,
    epoch: AtomicU64,
    lifecycle: AsyncMutex<()>,
    queue: Weak<Queue>,
    media: Arc<dyn Media>,
}
impl Automatic {
    pub fn new(
        devices: Arc<DeviceManager>,
        output: OutputSlot,
        queue: Weak<Queue>,
        local_source_url: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Status::default()),
            epoch: AtomicU64::new(0),
            lifecycle: AsyncMutex::new(()),
            queue,
            media: Arc::new(CoreMedia {
                devices,
                output,
                cached: Mutex::new(None),
                local_source_url,
            }),
        })
    }
    pub fn owns_output(&self) -> bool {
        self.state.lock().owns_output
    }
    pub fn ensure_manual(&self) -> Result<()> {
        ensure!(
            !self.owns_output(),
            "自动直播正在管理输出，请先停止自动直播"
        );
        Ok(())
    }
    pub async fn manual_guard(&self) -> Result<tokio::sync::MutexGuard<'_, ()>> {
        // Serialize manual start with automatic start, including queue-mode
        // changes after the media handle was installed.
        let guard = self.lifecycle.lock().await;
        self.ensure_manual()?;
        Ok(guard)
    }
    fn current(&self, epoch: u64) -> bool {
        let s = self.state.lock();
        self.epoch.load(Ordering::SeqCst) == epoch && s.owns_output && s.state != "stopping"
    }
    fn update(&self, epoch: u64, change: impl FnOnce(&mut Status)) -> bool {
        let mut s = self.state.lock();
        if self.epoch.load(Ordering::SeqCst) != epoch || !s.owns_output || s.state == "stopping" {
            return false;
        }
        change(&mut s);
        true
    }
    pub async fn start(&self, mut values: Value) -> Result<Value> {
        let _lifecycle = self.lifecycle.lock().await;
        ensure!(!self.owns_output(), "自动直播已经开始，请先停止");
        let fields = values.as_object_mut().context("自动直播配置无效")?;
        let device = fields
            .remove("device_id")
            .and_then(|v| v.as_str().map(str::to_owned))
            .filter(|v| !v.trim().is_empty())
            .context("请选择执行设备")?;
        let package = fields
            .remove("package_id")
            .and_then(|v| v.as_str().map(str::to_owned))
            .filter(|v| !v.trim().is_empty())
            .context("请选择配置包")?;
        let folder = fields
            .get("video_folder")
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .context("请填写服务端视频文件夹")?
            .to_owned();
        fields.insert("device_id".into(), json!(""));
        let request: OutputRequest = serde_json::from_value(values)?;
        request.validate()?;
        let queue = self.queue.upgrade().context("互动队列不可用")?;
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        *self.state.lock() = Status {
            state: "preparing".into(),
            video_folder: folder.clone(),
            device_id: device.clone(),
            package_id: package.clone(),
            error: None,
            owns_output: true,
            ready: false,
            video_source: true,
        };
        let previous = match queue.prepare_automatic(&device, &package, &folder).await {
            Ok(previous) => previous,
            Err(error) => {
                let mut s = self.state.lock();
                if self.epoch.load(Ordering::SeqCst) == epoch {
                    s.state = "failed".into();
                    s.error = Some(error.to_string());
                    s.owns_output = false;
                }
                return Err(error);
            }
        };
        let result = self.media.start(request).await;
        if !self.current(epoch) {
            anyhow::bail!("自动直播启动已停止");
        }
        if let Err(error) = result {
            queue.abort_automatic(previous).await?;
            self.update(epoch, |s| {
                s.state = "failed".into();
                s.error = Some(error.to_string());
                s.owns_output = false;
            });
            return Err(error);
        }
        if !self.update(epoch, |s| {
            s.ready = true;
            s.state = "video".into();
        }) {
            anyhow::bail!("自动直播启动已停止");
        }
        if let Err(error) = queue.activate_automatic().await {
            self.media.stop().await;
            queue.abort_automatic(previous).await?;
            self.update(epoch, |s| {
                s.state = "failed".into();
                s.error = Some(error.to_string());
                s.owns_output = false;
            });
            return Err(error);
        }
        if !self.current(epoch) {
            queue.suspend(false).await;
            anyhow::bail!("自动直播启动已停止");
        }
        self.status().await
    }
    /// Fence first, before waiting for a slow preparation or a running script.
    /// Queue stop cancels only the queue's own identity and awaits its cleanup.
    pub async fn stop(&self) -> Result<Value> {
        let queue = self.queue.upgrade();
        let stopped_epoch = {
            let mut s = self.state.lock();
            if s.owns_output {
                // Duplicate callers join the same stopping generation. A second
                // stop must neither invalidate its first caller nor reach into
                // a later session while waiting for the lifecycle lock.
                if s.state != "stopping" {
                    self.epoch.fetch_add(1, Ordering::SeqCst);
                }
                s.state = "stopping".into();
                if let Some(queue) = &queue {
                    queue.stop_admission();
                }
                Some(self.epoch.load(Ordering::SeqCst))
            } else {
                None
            }
        };
        let Some(stopped_epoch) = stopped_epoch else {
            return self.status().await;
        };
        let queue = queue.context("互动队列不可用")?;
        let _lifecycle = self.lifecycle.lock().await;
        let still_stopping = {
            let s = self.state.lock();
            self.epoch.load(Ordering::SeqCst) == stopped_epoch
                && s.owns_output
                && s.state == "stopping"
        };
        if !still_stopping {
            return self.status().await;
        }
        if let Err(error) = queue.stop_automatic().await {
            let mut s = self.state.lock();
            s.state = "paused".into();
            s.error = Some(error.to_string());
            return Err(error);
        }
        self.media.stop().await;
        {
            let mut s = self.state.lock();
            s.state = "stopped".into();
            s.error = None;
            s.owns_output = false;
            s.ready = false;
        }
        self.status().await
    }
    pub async fn status(&self) -> Result<Value> {
        let state = self.state.lock().clone();
        let mut status = json!(state);
        status["stream"] = self.media.status().await.unwrap_or(Value::Null);
        status["queue"] = if let Some(queue) = self.queue.upgrade() {
            queue.status(0, "").await
        } else {
            Value::Null
        };
        if !state.owns_output {
            if let Some(folder) = status["queue"]["automatic_video_folder"]
                .as_str()
                .map(str::to_owned)
            {
                status["video_folder"] = json!(folder);
            }
        }
        Ok(status)
    }
    /// Plugin/process teardown cannot leave transport resources running behind
    /// a disabled extension. Unlike the interactive stop, this also handles an
    /// unresolved historical run; its review record is retained by the queue.
    pub async fn shutdown(&self) {
        {
            let mut s = self.state.lock();
            self.epoch.fetch_add(1, Ordering::SeqCst);
            if s.owns_output {
                s.state = "stopping".into();
            }
            if let Some(queue) = self.queue.upgrade() {
                queue.stop_admission();
            }
        }
        let _lifecycle = self.lifecycle.lock().await;
        if let Some(queue) = self.queue.upgrade() {
            queue.suspend(false).await;
            queue.finish_preparation().await;
        }
        self.media.stop().await;
        let mut s = self.state.lock();
        s.state = "stopped".into();
        s.owns_output = false;
        s.ready = false;
    }
}
#[async_trait]
impl DispatchHooks for Automatic {
    async fn before_submit(&self, target: &Target) -> Result<()> {
        let epoch = self.epoch.load(Ordering::SeqCst);
        {
            let mut s = self.state.lock();
            if !s.owns_output {
                return Ok(());
            }
            ensure!(
                s.ready && s.state != "stopping",
                "自动直播正在停止或准备输出"
            );
            s.state = "preparing".into();
            s.error = None;
        }
        let result = self.media.device(&target.device_id).await;
        ensure!(self.current(epoch), "自动直播已停止，未启动脚本");
        if let Err(error) = result {
            self.update(epoch, |s| {
                s.state = "paused".into();
                s.error = Some(error.to_string());
            });
            return Err(error);
        }
        ensure!(
            self.update(epoch, |s| s.video_source = false),
            "自动直播已停止，未启动脚本"
        );
        Ok(())
    }
    fn executing(&self) {
        let mut s = self.state.lock();
        if s.owns_output && s.ready && s.state == "preparing" {
            s.state = "executing".into();
        }
    }
    async fn idle(&self, error: Option<String>) -> Result<()> {
        let epoch = self.epoch.load(Ordering::SeqCst);
        let restore = {
            let s = self.state.lock();
            if !s.owns_output || !s.ready || s.state == "stopping" {
                return Ok(());
            }
            !s.video_source
        };
        if restore {
            let result = self.media.video().await;
            if !self.current(epoch) {
                return Ok(());
            }
            if let Err(e) = result {
                self.update(epoch, |s| {
                    s.state = "paused".into();
                    s.error = Some(e.to_string());
                });
                return Err(e);
            }
        }
        if !self.current(epoch) {
            return Ok(());
        }
        let stream = self.media.reliable_status().await;
        if stream
            .as_ref()
            .is_none_or(|v| ["failed", "stopped"].contains(&v["state"].as_str().unwrap_or("")))
        {
            let message = stream
                .as_ref()
                .and_then(|v| v["error"].as_str())
                .unwrap_or("自动直播输出已结束，请停止后重新开始")
                .to_owned();
            self.update(epoch, |s| {
                s.state = "paused".into();
                s.error = Some(message.clone());
            });
            anyhow::bail!(message);
        }
        self.update(epoch, |s| {
            s.video_source = true;
            s.state = if error.is_some() { "paused" } else { "video" }.into();
            s.error = error;
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "automatic_tests.rs"]
mod tests;
