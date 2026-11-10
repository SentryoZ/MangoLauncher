//! Spawning and supervising the game process.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{error, info, warn};

use crate::error::{MangoError, Result};

/// Signature Minecraft prints once the game window is up, used to detect a
/// successful launch versus an immediate crash.
const GAME_LAUNCH_MARKERS: &[&str] =
    &["MinecraftForge", "ModLauncher", "with Fabric Loader", "Setting user:"];

/// Signature of the most common launch failure.
const LAUNCHWRAPPER_FAILURE: &str =
    "Could not find or load main class net.minecraft.launchwrapper.Launch";

/// Everything needed to start the JVM.
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub java_executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_dir: PathBuf,
    /// Detach so closing the launcher does not kill the game.
    pub detached: bool,
}

/// Handle to a running game process.
#[derive(Debug)]
pub struct GameProcess {
    pub child: Child,
    pub natives_dir: Option<PathBuf>,
}

impl GameProcess {
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Wait for the game to exit and clean up its natives directory.
    pub async fn wait(mut self) -> Result<Option<i32>> {
        let status: std::process::ExitStatus = self.child.wait().await?;
        if let Some(dir) = &self.natives_dir {
            if let Err(err) = std::fs::remove_dir_all(dir) {
                warn!(path = %dir.display(), error = %err, "could not remove natives dir");
            }
        }
        Ok(status.code())
    }

    /// Ask the game to exit.
    pub fn kill(&mut self) -> Result<()> {
        self.child.start_kill()?;
        Ok(())
    }
}

/// Start the game.
pub async fn spawn(request: LaunchRequest) -> Result<GameProcess> {
    if !request.java_executable.is_file() {
        return Err(MangoError::Other(format!(
            "java executable not found: {}",
            request.java_executable.display()
        )));
    }
    std::fs::create_dir_all(&request.working_dir)?;

    let mut command = Command::new(&request.java_executable);
    command
        .args(&request.arguments)
        .current_dir(&request.working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    if request.detached {
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // New process group so a terminal signal does not reach the game.
            unsafe {
                command.pre_exec(|| {
                    libc_setsid();
                    Ok(())
                });
            }
        }
    }

    info!(
        java = %request.java_executable.display(),
        cwd = %request.working_dir.display(),
        detached = request.detached,
        "starting minecraft"
    );
    // Log the resolved command with the access token masked; this is the line
    // support requests will be judged on.
    info!("launch arguments: {}", mask_arguments(&request.arguments).join(" "));

    let mut child =
        command.spawn().map_err(|err| MangoError::Other(format!("could not start java: {err}")))?;

    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(pipe_lines(stdout, "Minecraft", false));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(pipe_lines(stderr, "Minecraft", true));
    }

    Ok(GameProcess { child, natives_dir: None })
}

/// Replace the access token with asterisks so logs are safe to share.
pub fn mask_arguments(arguments: &[String]) -> Vec<String> {
    let mut masked = arguments.to_vec();
    for window in 0..masked.len() {
        let value = &masked[window];
        // Tokens are long, unpunctuated, and follow the access-token flag.
        let previous_is_flag = window > 0
            && matches!(
                masked[window - 1].trim_start_matches('-'),
                "accessToken" | "auth_access_token"
            );
        let looks_like_token = value.len() > 60
            && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if previous_is_flag || looks_like_token {
            masked[window] = "********".to_owned();
        }
    }
    masked
}

async fn pipe_lines<R>(reader: R, target: &'static str, is_error: bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.contains(LAUNCHWRAPPER_FAILURE) {
            error!(target, "launchwrapper failed: {line}");
            continue;
        }
        if is_error {
            warn!(target, "{line}");
        } else if GAME_LAUNCH_MARKERS.iter().any(|marker| line.contains(marker)) {
            info!(target, "game started: {line}");
        } else {
            info!(target, "{line}");
        }
    }
}

#[cfg(unix)]
fn libc_setsid() {
    // Avoid a libc dependency for one call; `setsid` is exposed by libSystem
    // and glibc through the standard symbol table.
    unsafe extern "C" {
        fn setsid() -> i32;
    }
    unsafe {
        setsid();
    }
}

/// Absolute path of the natives directory used for a launch.
pub fn natives_root(temp_dir: &Path) -> PathBuf {
    temp_dir.join("mango-natives")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_tokens_are_masked() {
        let args = vec![
            "--username".to_owned(),
            "Steve".to_owned(),
            "--accessToken".to_owned(),
            "eyJhbGciOiJIUzI1NiJ9.verylongtokenvalue".to_owned(),
            "--uuid".to_owned(),
            "abc".to_owned(),
        ];
        let masked = mask_arguments(&args);
        assert_eq!(masked[3], "********");
        assert_eq!(masked[1], "Steve");
        assert_eq!(masked[5], "abc");
    }

    #[test]
    fn long_single_token_arguments_are_masked_even_without_a_flag() {
        let token = "a".repeat(120);
        let masked = mask_arguments(std::slice::from_ref(&token));
        assert_eq!(masked[0], "********");
    }

    #[test]
    fn short_arguments_are_untouched() {
        let args = vec!["-Xmx4G".to_owned(), "-cp".to_owned(), "a.jar;b.jar".to_owned()];
        assert_eq!(mask_arguments(&args), args);
    }

    #[test]
    fn natives_root_is_under_temp() {
        assert_eq!(natives_root(Path::new("/tmp")), PathBuf::from("/tmp/mango-natives"));
    }

    #[tokio::test]
    async fn missing_java_executable_is_reported() {
        let request = LaunchRequest {
            java_executable: PathBuf::from("/definitely/not/java"),
            arguments: vec!["-version".to_owned()],
            working_dir: std::env::temp_dir(),
            detached: false,
        };
        let error = spawn(request).await.unwrap_err();
        assert!(error.to_string().contains("not found"));
    }
}
