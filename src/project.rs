use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug)]
pub struct ProjectInfo {
    pub name: String,
    pub path: PathBuf,
    pub repo_status: RepoStatus,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RepoStatus {
    pub available: bool,
    pub dirty: bool,
    pub ahead: usize,
    pub behind: usize,
    pub has_remote: bool,
    pub has_upstream: bool,
    pub remote_refresh_failed: bool,
}

impl RepoStatus {
    pub fn short_label(&self) -> String {
        if !self.available {
            return "status unavailable".to_string();
        }

        let mut parts = Vec::new();
        if self.dirty {
            parts.push("dirty".to_string());
        }
        if self.behind > 0 {
            parts.push(format!("behind {}", self.behind));
        }
        if self.ahead > 0 {
            parts.push(format!("ahead {}", self.ahead));
        }
        if self.remote_refresh_failed {
            parts.push("remote check failed".to_string());
        } else if self.has_remote && !self.has_upstream {
            parts.push("no upstream".to_string());
        } else if !self.has_remote {
            parts.push("local only".to_string());
        }

        if parts.is_empty() {
            "up to date".to_string()
        } else {
            parts.join(", ")
        }
    }

    pub fn css_class(&self) -> &'static str {
        if !self.available || self.remote_refresh_failed || self.behind > 0 || self.dirty {
            "repo-state-alert"
        } else if self.ahead > 0 || (self.has_remote && !self.has_upstream) {
            "repo-state-warn"
        } else if !self.has_remote {
            "repo-state-muted"
        } else {
            "repo-state-ok"
        }
    }

    pub fn needs_attention(&self) -> bool {
        !self.available || self.remote_refresh_failed || self.behind > 0 || self.dirty
    }
}

pub fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(cwd) = env::current_dir() {
        if let Some(parent) = cwd.parent() {
            roots.push(parent.to_path_buf());
        }
        roots.push(cwd);
    }

    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Documents").join("github"));
        roots.push(home.join("github"));
        roots.push(home.join("src"));
    }

    dedupe_paths(roots)
}

pub fn discover_projects(roots: &[PathBuf]) -> Vec<ProjectInfo> {
    let mut seen = HashSet::new();
    let mut projects = Vec::new();

    for root in roots {
        collect_project(root, &mut seen, &mut projects);
        for child in read_dirs(root) {
            if child.join(".git").exists() {
                collect_project(&child, &mut seen, &mut projects);
                continue;
            }

            for grandchild in read_dirs(&child) {
                collect_project(&grandchild, &mut seen, &mut projects);
            }
        }
    }

    projects.sort_by_key(|project| project.name.to_lowercase());
    projects
}

fn collect_project(path: &Path, seen: &mut HashSet<PathBuf>, projects: &mut Vec<ProjectInfo>) {
    if !path.is_dir() || !path.join(".git").exists() {
        return;
    }

    if !seen.insert(path.to_path_buf()) {
        return;
    }

    let name = path
        .file_name()
        .and_then(|part| part.to_str())
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string());

    projects.push(ProjectInfo {
        name,
        path: path.to_path_buf(),
        repo_status: RepoStatus::default(),
    });
}

pub fn inspect_project(path: &Path) -> RepoStatus {
    inspect_project_with_remote_refresh(path, true)
}

pub fn inspect_project_without_remote_refresh(path: &Path) -> RepoStatus {
    inspect_project_with_remote_refresh(path, false)
}

pub fn describe_pending_changes(path: &Path) -> Result<String, String> {
    let status = git_stdout_with_error(path, &["status", "--short"])?;
    if status.trim().is_empty() {
        return Ok("No uncommitted changes.".to_string());
    }

    let branch = git_stdout_with_error(path, &["branch", "--show-current"])
        .ok()
        .and_then(|output| {
            let branch = output.trim();
            (!branch.is_empty()).then(|| branch.to_string())
        })
        .unwrap_or_else(|| "detached HEAD".to_string());
    let has_head = git_stdout_with_error(path, &["rev-parse", "--verify", "HEAD"]).is_ok();
    let untracked = git_stdout_with_error(path, &["ls-files", "--others", "--exclude-standard"])
        .unwrap_or_default();

    let mut sections = vec![
        format!("Repository: {}\nBranch: {branch}", path.display()),
        "Commit+Push will run `git add -A`, so every path in `git status --short` is included."
            .to_string(),
        format!("Status\n{}", status.trim()),
    ];

    if has_head {
        let stat = git_stdout_with_error(path, &["diff", "--stat", "HEAD", "--"])?;
        if !stat.trim().is_empty() {
            sections.push(format!("Diffstat\n{}", stat.trim()));
        }

        if !untracked.trim().is_empty() {
            sections.push(format!("Untracked files\n{}", untracked.trim()));
        }

        let diff = git_stdout_with_error(
            path,
            &["diff", "--no-ext-diff", "--submodule=short", "HEAD", "--"],
        )?;
        sections.push(if diff.trim().is_empty() {
            "Tracked diff\nNo tracked file diff. Pending changes may be only untracked files."
                .to_string()
        } else {
            format!("Tracked diff\n{}", diff.trim())
        });
    } else {
        if !untracked.trim().is_empty() {
            sections.push(format!("Untracked files\n{}", untracked.trim()));
        }
        sections.push(
            "Tracked diff\nNo HEAD commit exists yet, so a diff against HEAD is unavailable."
                .to_string(),
        );
    }

    Ok(sections.join("\n\n"))
}

fn inspect_project_with_remote_refresh(path: &Path, refresh_remote: bool) -> RepoStatus {
    let has_remote = git_stdout(path, &["remote"])
        .ok()
        .is_some_and(|output| output.lines().any(|line| !line.trim().is_empty()));

    let remote_refresh_failed = refresh_remote
        && has_remote
        && git_stdout(
            path,
            &[
                "-c",
                "credential.interactive=never",
                "fetch",
                "--quiet",
                "--all",
                "--prune",
            ],
        )
        .is_err();

    let Ok(output) = git_stdout(path, &["status", "--porcelain=v2", "--branch"]) else {
        return RepoStatus {
            has_remote,
            remote_refresh_failed,
            ..RepoStatus::default()
        };
    };

    let mut status = parse_repo_status(&output);
    status.has_remote = has_remote;
    status.remote_refresh_failed = remote_refresh_failed;
    status
}

fn read_dirs(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .map(|entry| entry.path())
        .filter(|child| child.is_dir())
        .collect()
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for path in paths {
        if path.is_dir() && seen.insert(path.clone()) {
            deduped.push(path);
        }
    }
    deduped
}

fn parse_repo_status(output: &str) -> RepoStatus {
    let mut status = RepoStatus {
        available: true,
        ..RepoStatus::default()
    };

    for line in output.lines() {
        if let Some(upstream) = line.strip_prefix("# branch.upstream ") {
            status.has_upstream = !upstream.trim().is_empty();
            continue;
        }

        if let Some(ab) = line.strip_prefix("# branch.ab ") {
            for part in ab.split_whitespace() {
                if let Some(ahead) = part.strip_prefix('+') {
                    status.ahead = ahead.parse().unwrap_or(0);
                } else if let Some(behind) = part.strip_prefix('-') {
                    status.behind = behind.parse().unwrap_or(0);
                }
            }
            continue;
        }

        if let Some('1' | '2' | 'u' | '?') = line.chars().next() {
            status.dirty = true;
        }
    }

    status
}

fn git_stdout(path: &Path, args: &[&str]) -> Result<String, ()> {
    let output = Command::new("git")
        .current_dir(path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .map_err(|_| ())?;

    if !output.status.success() {
        return Err(());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn git_stdout_with_error(path: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .map_err(|error| {
            format!(
                "failed to run `git {}` in {}: {error}",
                args.join(" "),
                path.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "git {} exited with status {}{}{}",
            args.join(" "),
            output.status,
            if stderr.is_empty() { "" } else { "\n" },
            stderr
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn finds_git_repos_one_level_deep() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let base = env::temp_dir().join(format!("bellosaize-test-{unique}"));
        let repo = base.join("demo");

        fs::create_dir_all(&repo).expect("create repo dir");
        Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("init git repo");
        let projects = discover_projects(std::slice::from_ref(&base));

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "demo");

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn parses_clean_tracking_status() {
        let status = parse_repo_status(
            "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n",
        );

        assert_eq!(
            status,
            RepoStatus {
                available: true,
                dirty: false,
                ahead: 0,
                behind: 0,
                has_remote: false,
                has_upstream: true,
                remote_refresh_failed: false,
            }
        );
    }

    #[test]
    fn parses_dirty_diverged_status() {
        let status = parse_repo_status(
            "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -3\n1 .M N... 100644 100644 100644 abc abc src/app.rs\n? scratch.txt\n",
        );

        assert!(status.available);
        assert!(status.dirty);
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 3);
        assert!(status.has_upstream);
    }

    #[test]
    fn describes_pending_changes_for_dirty_repo() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        let repo = env::temp_dir().join(format!("bellosaize-changes-test-{unique}"));

        fs::create_dir_all(&repo).expect("create repo dir");
        Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("init git repo");
        fs::write(repo.join("tracked.txt"), "one\n").expect("write tracked");
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&repo)
            .output()
            .expect("git add");
        Command::new("git")
            .args([
                "-c",
                "user.name=BelloSaize Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ])
            .current_dir(&repo)
            .output()
            .expect("git commit");

        fs::write(repo.join("tracked.txt"), "two\n").expect("modify tracked");
        fs::write(repo.join("untracked.txt"), "new\n").expect("write untracked");

        let report = describe_pending_changes(&repo).expect("changes report");
        assert!(report.contains("Status"));
        assert!(report.contains("tracked.txt"));
        assert!(report.contains("untracked.txt"));
        assert!(report.contains("Tracked diff"));
        assert!(report.contains("-one"));
        assert!(report.contains("+two"));

        let _ = fs::remove_dir_all(repo);
    }
}
