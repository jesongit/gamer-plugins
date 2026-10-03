//! One tool dispatcher is shared by the local model and external MCP clients.
use super::{mcp::ToolResult, required, PauseReason, Session, State};
use crate::{
    capabilities::{
        AppId, DeviceHandle, DeviceId, FrameStamp, KeyAction, SwipeGesture, TextInput, TouchPoint,
    },
    extensions::Permission,
    targets::TargetCapabilities,
};
use anyhow::{ensure, Context, Result};
use image::{GenericImageView, ImageFormat};
use serde_json::{json, Value};
use std::{
    io::Cursor,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

#[derive(Clone)]
pub(super) struct Observation {
    pub frame_id: String,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub encoded_width: u32,
    pub encoded_height: u32,
    pub stamp: Option<FrameStamp>,
}
impl Observation {
    fn point(&self, args: &Value, x: &str, y: &str) -> Result<TouchPoint> {
        let (x, y) = (
            args[x].as_f64().context("坐标必须是有限数字")?,
            args[y].as_f64().context("坐标必须是有限数字")?,
        );
        ensure!(
            x.is_finite()
                && y.is_finite()
                && x >= 0.0
                && y >= 0.0
                && x < self.encoded_width as f64
                && y < self.encoded_height as f64,
            "坐标超出最近截图范围"
        );
        Ok(TouchPoint::new(
            (x * self.width as f64 / self.encoded_width as f64).floor() as u32,
            (y * self.height as f64 / self.encoded_height as f64).floor() as u32,
            1.0,
        ))
    }
}
fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn tool(
    name: &str,
    description: &str,
    mut properties: Value,
    required: &[&str],
    session: bool,
) -> Value {
    let mut fields = required.to_vec();
    if session {
        properties["session_id"] =
            json!({"type":"string","description":"session_status 返回的会话 ID"});
        properties["generation"] =
            json!({"type":"integer","minimum":1,"description":"session_status 返回的当前代次"});
        properties["operation_id"] = json!({"type":"string","maxLength":128,"description":"本次操作的唯一 ID（建议 UUID）；重试同一操作复用，新的操作必须使用新的 ID"});
        fields.extend(["session_id", "generation", "operation_id"]);
    }
    json!({"name":name,"description":description,"inputSchema":schema(properties,&fields)})
}
pub(super) fn catalog(c: TargetCapabilities, control: bool) -> Vec<Value> {
    let mut tools = vec![
        tool(
            "target_list",
            "查看令牌允许访问的目标与能力",
            json!({}),
            &[],
            false,
        ),
        tool(
            "context_get",
            "查看当前设备、应用和配置包上下文",
            json!({}),
            &[],
            false,
        ),
        tool(
            "session_status",
            "查看会话、控制状态与当前 generation",
            json!({}),
            &[],
            false,
        ),
    ];
    if c.frame {
        tools.push(tool("screen_capture","获取最新画面，返回真实图像及 frame_id；控制会话中请带 session_id/generation。坐标使用返回图像宽高。",json!({"session_id":{"type":"string"},"generation":{"type":"integer"},"max_width":{"type":"integer","minimum":320,"maximum":1920}}),&[],false));
    }
    if !control {
        return tools;
    }
    let point = json!({"frame_id":{"type":"string"},"x":{"type":"number","minimum":0},"y":{"type":"number","minimum":0}});
    if c.pointer {
        tools.push(tool(
            "input_tap",
            "点击最新截图内的坐标；完成后重新截图",
            point.clone(),
            &["frame_id", "x", "y"],
            true,
        ));
        let mut press = point;
        press["duration_ms"] = json!({"type":"integer","minimum":60,"maximum":3000});
        tools.push(tool(
            "input_press",
            "长按并可靠释放触点；完成后重新截图",
            press,
            &["frame_id", "x", "y", "duration_ms"],
            true,
        ));
        tools.push(tool("input_swipe","从起点滑动到终点并释放；坐标基于最近截图",json!({"frame_id":{"type":"string"},"x1":{"type":"number"},"y1":{"type":"number"},"x2":{"type":"number"},"y2":{"type":"number"},"duration_ms":{"type":"integer","minimum":100,"maximum":3000}}),&["frame_id","x1","y1","x2","y2","duration_ms"],true));
    }
    if c.keyboard {
        tools.push(tool("input_key","按下命名键后释放；如 Enter、Escape、ArrowUp、KeyW、BACK",json!({"frame_id":{"type":"string"},"key":{"type":"string"},"duration_ms":{"type":"integer","minimum":0,"maximum":3000}}),&["frame_id","key"],true));
        tools.push(tool(
            "input_text",
            "向当前输入框输入文本，内容不会写入事件日志",
            json!({"frame_id":{"type":"string"},"text":{"type":"string","maxLength":2000}}),
            &["frame_id", "text"],
            true,
        ));
    }
    if c.android_app {
        for (name, description) in [
            ("app_launch", "启动当前设备已配置的 Android 应用"),
            ("app_stop", "停止当前设备已配置的 Android 应用"),
        ] {
            tools.push(tool(
                name,
                description,
                json!({"frame_id":{"type":"string"}}),
                &["frame_id"],
                true,
            ));
        }
    }
    tools.push(tool(
        "wait",
        "等待最多 5 秒，随后重新观察",
        json!({"duration_ms":{"type":"integer","minimum":1,"maximum":5000}}),
        &["duration_ms"],
        true,
    ));
    tools.push(tool(
        "session_finish",
        "目标完成或无法继续时结束自动控制并说明结果",
        json!({"message":{"type":"string","maxLength":2000}}),
        &["message"],
        true,
    ));
    tools
}
pub(super) fn function_catalog(tools: &[Value]) -> Vec<Value> {
    tools.iter().filter(|t|t["name"].as_str()!=Some("target_list")).map(|t|{
        let mut parameters=t["inputSchema"].clone();if let Some(properties)=parameters["properties"].as_object_mut(){properties.remove("session_id");properties.remove("generation");properties.remove("operation_id");}
        if let Some(required)=parameters["required"].as_array_mut(){required.retain(|p|p!="session_id"&&p!="generation"&&p!="operation_id");}
        json!({"type":"function","name":t["name"],"description":t["description"],"parameters":parameters,"strict":false})
    }).collect()
}
pub(super) fn permission(name: &str) -> Result<Permission> {
    Ok(match name {
        "target_list" | "context_get" | "session_status" | "screen_capture" => {
            Permission::DeviceRead
        }
        "input_tap" => Permission::InputTap,
        "input_press" => Permission::Touch,
        "input_swipe" => Permission::InputSwipe,
        "input_key" => Permission::InputKey,
        "input_text" => Permission::InputText,
        "app_launch" | "app_stop" => Permission::DeviceApp,
        "wait" => Permission::RuntimeSleep,
        "session_finish" => Permission::UiHost,
        _ => anyhow::bail!("未知工具: {name}"),
    })
}
fn duration(args: &Value, default: u64, min: u64, max: u64) -> Result<Duration> {
    let millis = args.get("duration_ms").map_or(Ok(default), |v| {
        v.as_u64().context("duration_ms 必须为整数")
    })?;
    ensure!((min..=max).contains(&millis), "duration_ms 超出允许范围");
    Ok(Duration::from_millis(millis))
}

impl State {
    pub(super) async fn capture(
        &self,
        target: &str,
        generation: u64,
        max_width: u32,
    ) -> Result<(Observation, Value)> {
        self.authorize(Some(Permission::DeviceRead))?;
        let handle = DeviceHandle::new(DeviceId::new(target));
        let frames = self
            .runtime
            .capabilities
            .frame()
            .context("画面能力不可用")?;
        let before = if crate::targets::is_browser(target) {
            None
        } else {
            match frames.coordinate_space(&handle).await {
                Ok(space) => Some(space),
                Err(_) if generation == 0 => None,
                Err(error) => return Err(error.into()),
            }
        };
        let (bytes, stamp) = if crate::targets::is_browser(target) {
            let session = self.runtime.devices.browsers.session(target)?;
            let (bytes, stamp) = session.capture().await?;
            (bytes, Some(stamp))
        } else {
            let bytes = self.runtime.devices.screenshot(target).await?;
            if let Some((_, stamp)) = &before {
                ensure!(
                    frames.coordinate_space(&handle).await? == before.clone().unwrap(),
                    "stale_frame: 截图期间设备会话或尺寸改变，请重新观察"
                );
                (bytes, stamp.clone())
            } else {
                (bytes, None)
            }
        };
        ensure!(bytes.len() <= 28 * 1024 * 1024, "截图超过大小上限");
        let (png, width, height, encoded_width, encoded_height) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let image = image::load_from_memory_with_format(&bytes, ImageFormat::Png)?;
                let (width, height) = image.dimensions();
                ensure!(
                    width > 0 && height > 0 && width <= 8192 && height <= 8192,
                    "画面尺寸无效"
                );
                let image = if width > max_width {
                    image.resize(
                        max_width,
                        ((height as u64 * max_width as u64) / width as u64).max(1) as u32,
                        image::imageops::FilterType::Triangle,
                    )
                } else {
                    image
                };
                let (w, h) = image.dimensions();
                let mut png = Cursor::new(Vec::new());
                image.write_to(&mut png, ImageFormat::Png)?;
                Ok((png.into_inner(), width, height, w, h))
            })
            .await??;
        if let Some((size, _)) = before {
            ensure!(
                size.width == width && size.height == height,
                "stale_frame: 截图与输入坐标空间不同，请重新观察"
            );
        }
        let observation = Observation {
            frame_id: uuid::Uuid::new_v4().to_string(),
            generation,
            width,
            height,
            encoded_width,
            encoded_height,
            stamp,
        };
        let result=ToolResult::image(&png,"image/png",json!({"frame_id":observation.frame_id,"generation":generation,"device_id":target,"captured_at":chrono::Utc::now().to_rfc3339(),"width":encoded_width,"height":encoded_height,"original_width":width,"original_height":height})).value();
        Ok((observation, result))
    }
    pub(super) async fn tool(
        self: &Arc<Self>,
        session: &Arc<Session>,
        name: &str,
        args: Value,
        call_id: &str,
        generation: u64,
    ) -> Result<Value> {
        // The operation owns its task: dropping a HTTP caller cannot abandon a
        // DOWN before UP. Pause/stop still invalidate queued work and drain Core.
        let (state, owned_session, name, call_id) = (
            self.clone(),
            session.clone(),
            name.to_owned(),
            call_id.to_owned(),
        );
        let diagnostic_name = name.clone();
        let diagnostic_id = call_id.clone();
        let diagnostic_args = public_arguments(&name, &args);
        let result = tokio::spawn(async move {
            state
                .tool_inner(&owned_session, &name, args, &call_id, generation)
                .await
        })
        .await
        .context("工具任务异常结束")?;
        if let Err(error) = &result {
            let already_recorded = session.record.lock().events.iter().any(|event| {
                event.kind == "tool"
                    && event.data["phase"] == "result"
                    && event.data["call_id"] == diagnostic_id
                    && event.data["generation"] == generation
            });
            if !already_recorded {
                session.event("tool", format!("工具 {diagnostic_name} 未完成：{error}"), json!({"phase":"result","tool":diagnostic_name,"call_id":diagnostic_id,"generation":generation,"arguments":diagnostic_args,"ok":false,"result":{"error":error.to_string()}}));
            }
            if error.to_string().contains("target_changed:") {
                self.pause_automatic(
                    session,
                    generation,
                    PauseReason::new(
                        "target_changed",
                        "target",
                        "目标连接发生变化",
                        error.to_string(),
                        "确认连接的目标后继续；浏览器绑定已更换时请开始新对话。",
                        true,
                    ),
                )
                .await?;
            }
        }
        result
    }
    async fn tool_inner(
        &self,
        session: &Arc<Session>,
        name: &str,
        args: Value,
        call_id: &str,
        generation: u64,
    ) -> Result<Value> {
        self.authorize(Some(permission(name)?))?;
        ensure!(args.is_object(), "工具参数必须是对象");
        let _operation = session.operation.lock().await;
        session.charge_time();
        let record = session.record.lock().clone();
        ensure!(
            record.state == "running"
                && record.generation == generation
                && !session.ending.load(Ordering::Acquire),
            "stale_generation: 会话已暂停、停止或代次变化"
        );
        let key = format!("{generation}:{call_id}");
        let fingerprint = serde_json::to_string(&json!([name, args]))?;
        if let Some(cached) = session.results.lock().get(&key).cloned() {
            ensure!(
                cached["fingerprint"].as_str() == Some(&fingerprint),
                "重复 call_id 的工具参数不一致"
            );
            return Ok(cached["result"].clone());
        }
        let c = crate::targets::capabilities(&self.runtime.devices, &record.device_id)?;
        ensure!(
            catalog(c, true)
                .iter()
                .any(|t| t["name"].as_str() == Some(name)),
            "当前目标不支持工具 {name}"
        );
        if name == "session_status" {
            return Ok(ToolResult::json(json!({"session":record,"input_control":self.runtime.devices.controls.status(&record.device_id)})).value());
        }
        if name == "context_get" || name == "target_list" {
            return Ok(ToolResult::json(json!({"device_id":record.device_id,"content_package":record.content_package,"android_package":record.android_package,"capabilities":c})).value());
        }
        ensure!(
            record.usage.actions < record.limits.max_actions,
            "工具调用达到预算，请暂停处理"
        );
        ensure!(
            record.usage.active_seconds < record.limits.max_seconds as f64
                && (record.limits.max_tokens == 0
                    || (record.usage.known_tokens < record.limits.max_tokens
                        && record
                            .usage
                            .total_tokens
                            .is_none_or(|t| t < record.limits.max_tokens))),
            "活动时长或 token 使用达到预算，请暂停处理"
        );
        let lease = session
            .lease
            .lock()
            .await
            .as_ref()
            .context("控制会话未就绪")?
            .clone();
        // Pause/resume may replace the lease while this request waits for it.
        // Never let an old request borrow the resumed generation's authority.
        ensure!(
            lease.generation == generation
                && lease.owner == record.session_id
                && lease.target == record.device_id,
            "stale_generation: 控制租约已换代，请使用当前会话代次"
        );
        let handle = DeviceHandle::new(DeviceId::new(&record.device_id));
        session.event("tool", format!("正在执行 {name}"), json!({"phase":"start","tool":name,"call_id":call_id,"generation":generation,"arguments":public_arguments(name,&args)}));
        let result=self.runtime.devices.controls.execute(&lease,async {
            if name=="screen_capture" {
                let max_width=args.get("max_width").map_or(Ok(1280u32),|v|v.as_u64().and_then(|n|u32::try_from(n).ok()).context("max_width 必须为整数"))?;ensure!((320..=1920).contains(&max_width),"max_width 超出允许范围");
                let (frame,mut result)=self.capture(&record.device_id,generation,max_width).await?;
                if let (Some(previous),Some(current))=(session.binding.lock().as_ref(),frame.stamp.as_ref()){ensure!(previous.target==current.target&&previous.epoch==current.epoch,"target_changed: 连接已重建，请先暂停并确认目标");}
                result["structuredContent"]["session_id"]=json!(record.session_id);*session.frame.lock()=Some(frame.clone());
                if let Some(stamp)=&frame.stamp {*session.binding.lock()=Some(stamp.clone());}
                let preview=result["content"].as_array().and_then(|content|content.iter().find(|c|c["type"]=="image")).map(|image|format!("data:image/png;base64,{}",image["data"].as_str().unwrap_or("")));
                session.event("observation","已观察最新画面",json!({"frame_id":frame.frame_id,"width":frame.encoded_width,"height":frame.encoded_height,"image_data_url":preview}));return Ok(result);
            }
            if name=="wait" {let wait=duration(&args,500,1,5000)?;let cancel=session.cancelled.lock().clone();tokio::select!{_=tokio::time::sleep(wait)=>{},_=async{while !cancel.load(Ordering::Acquire){tokio::time::sleep(Duration::from_millis(25)).await;}}=>{}};return Ok(ToolResult::json(json!({"ok":true})).value());}
            if name=="session_finish" {let message=required(&args,"message")?;ensure!(message.len()<=8000,"结束说明过长");session.event("assistant",message,json!({}));session.ending.store(true,Ordering::Release);session.wake.notify_waiters();return Ok(ToolResult::json(json!({"ok":true,"finished":true})).value());}
            let frame=session.frame.lock().clone().context("请先调用 screen_capture 观察最新画面")?;
            ensure!(frame.generation==generation&&args["frame_id"].as_str()==Some(frame.frame_id.as_str()),"stale_frame: 请重新截图后操作");
            let service=self.runtime.capabilities.frame().context("画面能力不可用")?;let(size,stamp)=service.coordinate_space(&handle).await?;
            ensure!(size.width==frame.width&&size.height==frame.height&&stamp==frame.stamp,"stale_frame: 目标或画面尺寸已改变，请重新截图");
            let handle=frame.stamp.as_ref().map_or_else(||handle.clone(),|stamp|handle.clone().with_expected_frame(stamp.clone()));
            let app=crate::targets::app_context(&self.runtime.devices,&record.device_id,Some(crate::core::AppPackageId::new(&record.content_package)?))?;ensure!(app.android_package.as_ref().map(|p|p.as_str())==record.android_package.as_deref(),"运行目标应用已改变，请暂停并停止后重新开始");
            let input=self.runtime.capabilities.input().context("输入能力不可用")?;
            match name {
                "input_tap"=>input.tap_from_frame(&handle,frame.point(&args,"x","y")?,frame.stamp.as_ref()).await?,
                "input_swipe"=>input.swipe_from_frame(&handle,SwipeGesture::new(frame.point(&args,"x1","y1")?,frame.point(&args,"x2","y2")?,duration(&args,500,100,3000)?),frame.stamp.as_ref()).await?,
                "input_press"=> {
                    let point=frame.point(&args,"x","y")?;let wait=duration(&args,500,60,3000)?;
                    if crate::targets::is_browser(&record.device_id){let browser=self.runtime.devices.browsers.session(&record.device_id)?;browser.input(&json!({"type":"pointer","action":"down","x":point.x(),"y":point.y()}),frame.stamp.as_ref()).await?;tokio::time::sleep(wait).await;browser.input(&json!({"type":"pointer","action":"up","x":point.x(),"y":point.y()}),frame.stamp.as_ref()).await?;}
                    else {let touch=self.runtime.capabilities.touch().context("触控能力不可用")?;let contact=touch.begin(&handle,point).await?;tokio::time::sleep(wait).await;touch.end(&contact).await?;}
                },
                "input_key"=> {let key=required(&args,"key")?;ensure!(key.len()<=80,"按键名称过长");let wait=duration(&args,0,0,3000)?;if wait.is_zero(){input.key_named(&handle,key,KeyAction::Press).await?;}else {input.key_named(&handle,key,KeyAction::Down).await?;tokio::time::sleep(wait).await;input.key_named(&handle,key,KeyAction::Up).await?;}},
                "input_text"=>{let text=required(&args,"text")?;ensure!(text.chars().count()<=2000,"文本超过 2000 字符");input.text(&handle,TextInput::new(text)).await?;},
                "app_launch"|"app_stop"=> {let app=AppId::new(record.android_package.as_ref().context("当前设备没有配置应用")?);let device=self.runtime.capabilities.device().context("应用能力不可用")?;if name=="app_launch"{device.start_app(&handle,&app).await?;}else{device.stop_app(&handle,&app).await?;}},
                _=>anyhow::bail!("未知操作"),
            }
            session.frame.lock().take();Ok(ToolResult::json(json!({"ok":true,"observe_again":true})).value())
        }).await;
        session.record.lock().usage.actions += 1;
        session.event("tool", if result.is_ok() { format!("已执行 {name}") } else { format!("工具 {name} 执行失败") }, json!({"phase":"result","tool":name,"call_id":call_id,"generation":generation,"arguments":public_arguments(name,&args),"ok":result.is_ok(),"result":match &result {Ok(value)=>public_result(value),Err(error)=>json!({"error":error.to_string()})}}));
        // Cache failures too: retrying a timed-out call must never inject twice.
        let cached = match &result {
            Ok(value) => value.clone(),
            Err(error) => ToolResult::error(error.to_string()).value(),
        };
        let mut results = session.results.lock();
        // Keep mutation receipts for the entire bounded generation. Only the
        // latest screenshot retains pixels; old captures cannot be used again.
        if name == "screen_capture" {
            for value in results.values_mut() {
                if value["result"]["content"]
                    .as_array()
                    .is_some_and(|content| content.iter().any(|c| c["type"] == "image"))
                {
                    value["result"] =
                        ToolResult::error("截图调用已过期，请用新的 call_id 重新截图").value();
                }
            }
        }
        results.insert(key, json!({"fingerprint":fingerprint,"result":cached}));
        result
    }
}

fn public_arguments(name: &str, args: &Value) -> Value {
    let mut public = args.clone();
    if name == "input_text" {
        if let Some(object) = public.as_object_mut() {
            object.insert("text".into(), json!("[已隐藏]"));
        } else {
            return json!({"invalid_args":true});
        }
    }
    public
}

/// Timeline receipts carry metadata and errors, never raw image pixels.
fn public_result(result: &Value) -> Value {
    let content = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|block| {
            if block["type"] == "image" {
                json!({"type":"image","mimeType":block["mimeType"],"preview":"见画面观察记录"})
            } else {
                block.clone()
            }
        })
        .collect::<Vec<_>>();
    json!({"content":content,"structuredContent":result["structuredContent"],"isError":result["isError"]})
}

#[cfg(test)]
mod timeline_tests {
    use super::*;

    #[test]
    fn text_receipts_hide_typed_contents_and_image_receipts_only_retain_metadata() {
        let args = public_arguments(
            "input_text",
            &json!({"frame_id":"f","text":"private typed content"}),
        );
        assert_eq!(args["frame_id"], "f");
        assert_eq!(args["text"], "[已隐藏]");
        assert!(!args.to_string().contains("private typed content"));
        let invalid = public_arguments("input_text", &json!("private malformed content"));
        assert_eq!(invalid["invalid_args"], true);
        assert!(!invalid.to_string().contains("private malformed content"));
        let result = public_result(
            &ToolResult::image(&[1, 2, 3], "image/png", json!({"frame_id":"f"})).value(),
        );
        assert_eq!(result["structuredContent"]["frame_id"], "f");
        assert_eq!(result["content"][1]["type"], "image");
        assert!(result["content"][1].get("data").is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coordinate_mapping_rejects_edges_and_non_numbers() {
        let f = Observation {
            frame_id: "f".into(),
            generation: 1,
            width: 1920,
            height: 1080,
            encoded_width: 1280,
            encoded_height: 720,
            stamp: None,
        };
        let p = f.point(&json!({"x":640,"y":360}), "x", "y").unwrap();
        assert_eq!((p.x(), p.y()), (960, 540));
        assert!(f.point(&json!({"x":1280,"y":0}), "x", "y").is_err());
        assert!(f.point(&json!({"x":-1,"y":0}), "x", "y").is_err());
    }
    #[test]
    fn catalog_filters_target_and_read_only_scope() {
        let browser = catalog(TargetCapabilities::browser(), true);
        assert!(!browser.iter().any(|t| t["name"] == "app_launch"));
        assert!(catalog(TargetCapabilities::android(), false)
            .iter()
            .all(|t| !t["name"].as_str().unwrap().starts_with("input_")));
        for f in function_catalog(&browser) {
            assert!(f["parameters"]["properties"].get("generation").is_none());
        }
    }
}
