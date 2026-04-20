use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct ProjectInfo {
    pub name: String,
    pub path: PathBuf,
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

    projects.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
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
    });
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

        fs::create_dir_all(repo.join(".git")).expect("create fake git repo");
        let projects = discover_projects(std::slice::from_ref(&base));

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "demo");

        let _ = fs::remove_dir_all(base);
    }
}
