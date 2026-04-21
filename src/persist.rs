use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    #[default]
    Shell,
    Codex,
    Claude,
    Mistral,
    Custom,
}

impl Profile {
    pub fn label(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Mistral => "mistral",
            Self::Custom => "custom",
        }
    }

    pub fn default_command(self) -> String {
        match self {
            Self::Shell => default_shell(),
            Self::Codex => "codex --dangerously-bypass-approvals-and-sandbox".to_string(),
            Self::Claude => "claude".to_string(),
            Self::Mistral => "mistral".to_string(),
            Self::Custom => String::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SessionSpec {
    pub cwd: PathBuf,
    pub command: String,
    pub name: Option<String>,
    #[serde(default)]
    pub profile: Profile,
}

impl SessionSpec {
    pub fn title(&self) -> String {
        if let Some(name) = self.name.as_ref().filter(|value| !value.trim().is_empty()) {
            return name.clone();
        }

        self.cwd
            .file_name()
            .and_then(|part| part.to_str())
            .filter(|part| !part.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.cwd.display().to_string())
    }

    pub fn subtitle(&self) -> String {
        let command = self.command_label();
        format!("{}  |  {}", self.cwd.display(), command)
    }

    pub fn command_label(&self) -> String {
        if self.command.trim().is_empty() {
            return default_shell();
        }

        shlex::split(&self.command)
            .and_then(|parts| parts.into_iter().next())
            .filter(|part| !part.is_empty())
            .unwrap_or_else(default_shell)
    }

    pub fn resolved_command(&self) -> String {
        if self.command.trim().is_empty() {
            self.profile.default_command()
        } else {
            self.command.clone()
        }
    }

    pub fn normalized(&self) -> Result<Self> {
        let cwd = normalize_cwd(&self.cwd)?;
        let mut spec = self.clone();
        spec.cwd = cwd;
        if spec.command.trim().is_empty() {
            spec.command = spec.profile.default_command();
        }
        Ok(spec)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SessionFile {
    #[serde(default)]
    pub sessions: Vec<SessionSpec>,
}

pub fn load_or_bootstrap(default_cwd: &Path) -> Result<(SessionFile, PathBuf)> {
    let path = session_path()?;
    if path.exists() {
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read session file at {}", path.display()))?;
        let session = toml::from_str::<SessionFile>(&raw)
            .with_context(|| format!("failed to parse session file at {}", path.display()))?;
        return Ok((session, path));
    }

    let session = SessionFile {
        sessions: Vec::new(),
    };
    save(&path, &session)?;
    if !default_cwd.exists() {
        return Err(anyhow!(
            "default working directory does not exist: {}",
            default_cwd.display()
        ));
    }
    Ok((session, path))
}

pub fn save(path: &Path, session: &SessionFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }

    let contents = toml::to_string_pretty(session).context("failed to serialize session file")?;
    fs::write(path, contents)
        .with_context(|| format!("failed to write session file {}", path.display()))?;
    Ok(())
}

pub fn default_shell() -> String {
    env::var("SHELL").unwrap_or_else(|_| "bash".to_string())
}

fn normalize_cwd(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return env::current_dir().context("failed to resolve current directory");
    }

    if path.is_dir() {
        return Ok(path.to_path_buf());
    }

    Err(anyhow!("{} is not a directory", path.display()))
}

fn session_path() -> Result<PathBuf> {
    let base = dirs::config_dir().context("unable to resolve config directory")?;
    Ok(base.join("bellosaize").join("session.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_defaults_are_stable() {
        assert_eq!(Profile::Shell.default_command(), default_shell());
        assert_eq!(
            Profile::Codex.default_command(),
            "codex --dangerously-bypass-approvals-and-sandbox"
        );
        assert_eq!(Profile::Claude.default_command(), "claude");
        assert_eq!(Profile::Mistral.default_command(), "mistral");
    }
}
