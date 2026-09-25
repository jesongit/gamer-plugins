//! Only the publisher calls this adapter; no command strings reach a shell.
use anyhow::{bail, ensure, Context, Result};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

struct OutputDir(PathBuf);
impl Drop for OutputDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn run(args: &[String], root: &Path, cancelled: &AtomicBool) -> Result<Vec<u8>> {
    let mut command = Command::new("gh");
    command
        .args(args)
        .env("GH_HOST", "github.com")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_PAGER", "cat");
    run_command(command, root, cancelled, Duration::from_secs(120))
}

fn run_command(
    mut command: Command,
    root: &Path,
    cancelled: &AtomicBool,
    timeout: Duration,
) -> Result<Vec<u8>> {
    ensure!(!cancelled.load(Ordering::SeqCst), "操作已取消");
    let dir = OutputDir(root.join(format!("gh-output-{}", uuid::Uuid::new_v4())));
    fs::create_dir_all(&dir.0)?;
    let output = dir.0.join("stdout");
    let errors = dir.0.join("stderr");
    command
        .stdin(Stdio::null())
        .stdout(File::create(&output)?)
        .stderr(File::create(&errors)?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .context("无法启动 gh，请在运行 Gamer 的电脑上安装 GitHub CLI 并登录")?;
    let start = Instant::now();
    loop {
        let exceeded = [&output, &errors].iter().any(|p| {
            fs::metadata(p)
                .map(|m| m.len() > 2 * 1024 * 1024)
                .unwrap_or(false)
        });
        if cancelled.load(Ordering::SeqCst) || start.elapsed() > timeout || exceeded {
            let _ = child.kill();
            let _ = child.wait();
            bail!("gh 操作已取消、超时或输出超限；远端草稿可能已创建，可重试核对");
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // Never return gh stderr: credentials and authenticated URLs may be present.
                ensure!(status.success(), "gh 操作失败，请检查登录、网络和目标仓库写权限；可在终端使用 gh auth status 排查");
                return crate::package_market::read_limited(File::open(&output)?, 2 * 1024 * 1024);
            }
            Ok(None) => thread::sleep(Duration::from_millis(40)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_prevents_spawn() {
        let d = tempfile::tempdir().unwrap();
        assert!(run_command(
            Command::new("not-a-real-command"),
            d.path(),
            &AtomicBool::new(true),
            Duration::from_secs(1)
        )
        .unwrap_err()
        .to_string()
        .contains("取消"));
    }
    #[test]
    fn timeout_kills_and_reaps_child() {
        let d = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let mut command = Command::new("powershell.exe");
        #[cfg(windows)]
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 20",
        ]);
        #[cfg(not(windows))]
        let mut command = Command::new("sleep");
        #[cfg(not(windows))]
        command.arg("20");
        let start = Instant::now();
        assert!(run_command(
            command,
            d.path(),
            &AtomicBool::new(false),
            Duration::from_millis(100)
        )
        .is_err());
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0);
    }
}
