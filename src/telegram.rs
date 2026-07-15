use std::{error::Error, fmt, process::Command};

const DEFAULT_PROXY_URL: &str = "http://127.0.0.1:7890";

#[derive(Debug, Clone)]
pub struct TelegramNotifier {
    bot_token: String,
    chat_id: String,
    proxy_url: Option<String>,
}

impl TelegramNotifier {
    pub fn from_env() -> Result<Self, TelegramError> {
        let bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
            .map_err(|_| TelegramError::MissingEnv("TELEGRAM_BOT_TOKEN"))?;
        let chat_id = std::env::var("TELEGRAM_CHAT_ID")
            .map_err(|_| TelegramError::MissingEnv("TELEGRAM_CHAT_ID"))?;
        let proxy_url = std::env::var("TELEGRAM_PROXY_URL")
            .or_else(|_| std::env::var("POLYMARKET_PROXY_URL"))
            .ok()
            .and_then(normalize_proxy_url)
            .or_else(|| Some(DEFAULT_PROXY_URL.to_owned()));

        Ok(Self {
            bot_token,
            chat_id,
            proxy_url,
        })
    }

    pub fn send_message(&self, text: &str) -> Result<(), TelegramError> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let mut command = Command::new("curl");
        command.args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--connect-timeout",
            "4",
            "--max-time",
            "8",
            "--request",
            "POST",
            &url,
            "--data-urlencode",
            &format!("chat_id={}", self.chat_id),
            "--data-urlencode",
            &format!("text={text}"),
            "--data-urlencode",
            "disable_web_page_preview=true",
        ]);

        if let Some(proxy_url) = &self.proxy_url {
            command.args(["--proxy", proxy_url]);
        }

        let output = command.output().map_err(TelegramError::Command)?;

        if output.status.success() {
            Ok(())
        } else {
            Err(TelegramError::Status {
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            })
        }
    }
}

#[derive(Debug)]
pub enum TelegramError {
    MissingEnv(&'static str),
    Command(std::io::Error),
    Status { code: Option<i32>, stderr: String },
}

impl fmt::Display for TelegramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnv(name) => write!(f, "missing environment variable {name}"),
            Self::Command(error) => write!(f, "failed to run curl for Telegram: {error}"),
            Self::Status { code, stderr } => {
                write!(f, "Telegram curl exited with code {code:?}: {stderr}")
            }
        }
    }
}

impl Error for TelegramError {}

fn normalize_proxy_url(proxy_url: String) -> Option<String> {
    let proxy_url = proxy_url.trim();

    if proxy_url.is_empty()
        || proxy_url.eq_ignore_ascii_case("none")
        || proxy_url.eq_ignore_ascii_case("off")
        || proxy_url.eq_ignore_ascii_case("direct")
    {
        None
    } else {
        Some(proxy_url.to_owned())
    }
}
