use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
};

use anyhow::{Context, Result, anyhow};
use gio::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CenterBox, CssProvider, DropDown,
    Entry, Grid, Label, ListBox, Orientation, Paned, ResponseType, ScrolledWindow, Stack,
    TextBuffer, TextView, gdk, prelude::*,
};
use pango::{EllipsizeMode, FontDescription};
use vte::{CursorBlinkMode, PtyFlags, Terminal, prelude::*};

use crate::{
    persist::{Profile, SessionFile, SessionSpec, load_or_bootstrap, save},
    project::{
        ProjectInfo, RepoStatus, default_roots, discover_projects, inspect_project,
        inspect_project_without_remote_refresh,
    },
};

const APP_ID: &str = "com.mmdmcy.BelloSaize";

#[derive(Clone, Copy, Eq, PartialEq)]
enum RepoActionScope {
    Selected,
    All,
}

impl RepoActionScope {
    fn from_index(index: u32) -> Self {
        match index {
            1 => Self::All,
            _ => Self::Selected,
        }
    }
}

#[derive(Clone, Copy)]
enum RepoAction {
    Fetch,
    Pull,
}

impl RepoAction {
    fn label(self) -> &'static str {
        match self {
            Self::Fetch => "Fetch",
            Self::Pull => "Pull",
        }
    }

    fn status_verb(self) -> &'static str {
        match self {
            Self::Fetch => "fetched",
            Self::Pull => "pulled",
        }
    }
}

#[derive(Clone, Copy)]
enum RepoActionOutcome {
    Applied,
    Skipped,
    Failed,
}

#[derive(Clone, Copy)]
enum CommitPushMode {
    CommitAndPush,
    PushOnly,
}

impl CommitPushMode {
    fn dialog_title(self) -> &'static str {
        match self {
            Self::CommitAndPush => "Commit And Push",
            Self::PushOnly => "Push Local Commits",
        }
    }

    fn report_title(self) -> &'static str {
        match self {
            Self::CommitAndPush => "Commit+Push",
            Self::PushOnly => "Push",
        }
    }
}

struct RepoActionReport {
    name: String,
    path: PathBuf,
    repo_status: RepoStatus,
    output: String,
    outcome: RepoActionOutcome,
}

#[derive(Clone)]
struct CommitPushTarget {
    cwd: PathBuf,
    title: String,
}

pub fn run() -> Result<()> {
    let application = Application::builder().application_id(APP_ID).build();
    application.connect_activate(|app| {
        if let Err(error) = build_ui(app) {
            eprintln!("failed to start BelloSaize: {error:#}");
        }
    });
    application.run();
    Ok(())
}

type SharedState = Rc<RefCell<AppState>>;

struct AppState {
    window: ApplicationWindow,
    stack: Stack,
    grid: Grid,
    root_paned: Paned,
    sidebar: GtkBox,
    sidebar_toggle_button: Button,
    project_list: ListBox,
    project_count_label: Label,
    workspace_title_label: Label,
    workspace_path_label: Label,
    workspace_repo_status_label: Label,
    status_label: Label,
    count_label: Label,
    reset_button: Button,
    close_button: Button,
    zoom_button: Button,
    repo_scope_combo: DropDown,
    fetch_button: Button,
    pull_button: Button,
    commit_push_button: Button,
    session_file_path: PathBuf,
    project_roots: Vec<PathBuf>,
    projects: Vec<ProjectInfo>,
    selected_project_index: Option<usize>,
    sessions: Vec<Rc<SessionView>>,
    selected_session_id: Option<u64>,
    zoomed_session_id: Option<u64>,
    sidebar_visible: bool,
    last_sidebar_width: i32,
    next_session_id: u64,
}

struct SessionView {
    id: u64,
    spec: RefCell<SessionSpec>,
    card: GtkBox,
    subtitle_label: Label,
    status_label: Label,
    terminal: Terminal,
    pid: Cell<Option<glib::Pid>>,
    alive: Cell<bool>,
}

#[derive(Default)]
struct TerminalClickState {
    press_x: Cell<f64>,
    press_y: Cell<f64>,
    clear_selection_on_release: Cell<bool>,
}

struct LaunchButtons {
    shell_button: Button,
    codex_button: Button,
    claude_button: Button,
    mistral_button: Button,
    custom_button: Button,
}

struct SidebarWidgets {
    project_list: ListBox,
    project_count_label: Label,
    refresh_button: Button,
    repo_scope_combo: DropDown,
    fetch_button: Button,
    pull_button: Button,
    commit_push_button: Button,
}

struct WorkspaceWidgets {
    stack: Stack,
    grid: Grid,
    sidebar_toggle_button: Button,
    workspace_title_label: Label,
    workspace_path_label: Label,
    workspace_repo_status_label: Label,
    status_label: Label,
    count_label: Label,
    reset_button: Button,
    launch_buttons: LaunchButtons,
    close_button: Button,
    zoom_button: Button,
}

fn build_ui(application: &Application) -> Result<()> {
    let cwd = env::current_dir().context("failed to resolve current working directory")?;
    let (_, session_file_path) = load_or_bootstrap(&cwd)?;
    let project_roots = default_roots();

    let window = ApplicationWindow::builder()
        .application(application)
        .title("BelloSaize")
        .default_width(1360)
        .default_height(960)
        .build();

    let (sidebar, sidebar_widgets) = build_sidebar();
    let (workspace, workspace_widgets) = build_workspace();

    let root = Paned::new(Orientation::Horizontal);
    root.add_css_class("app-shell");
    root.set_wide_handle(false);
    root.set_shrink_start_child(false);
    root.set_position(260);
    root.set_start_child(Some(&sidebar));
    root.set_end_child(Some(&workspace));

    window.set_child(Some(&root));
    apply_css();

    let state = Rc::new(RefCell::new(AppState {
        window: window.clone(),
        stack: workspace_widgets.stack,
        grid: workspace_widgets.grid,
        root_paned: root.clone(),
        sidebar: sidebar.clone(),
        sidebar_toggle_button: workspace_widgets.sidebar_toggle_button.clone(),
        project_list: sidebar_widgets.project_list.clone(),
        project_count_label: sidebar_widgets.project_count_label,
        workspace_title_label: workspace_widgets.workspace_title_label,
        workspace_path_label: workspace_widgets.workspace_path_label,
        workspace_repo_status_label: workspace_widgets.workspace_repo_status_label,
        status_label: workspace_widgets.status_label,
        count_label: workspace_widgets.count_label,
        reset_button: workspace_widgets.reset_button.clone(),
        close_button: workspace_widgets.close_button.clone(),
        zoom_button: workspace_widgets.zoom_button.clone(),
        repo_scope_combo: sidebar_widgets.repo_scope_combo.clone(),
        fetch_button: sidebar_widgets.fetch_button.clone(),
        pull_button: sidebar_widgets.pull_button.clone(),
        commit_push_button: sidebar_widgets.commit_push_button.clone(),
        session_file_path,
        project_roots,
        projects: Vec::new(),
        selected_project_index: None,
        sessions: Vec::new(),
        selected_session_id: None,
        zoomed_session_id: None,
        sidebar_visible: true,
        last_sidebar_width: 260,
        next_session_id: 1,
    }));

    {
        let state = state.clone();
        workspace_widgets
            .launch_buttons
            .shell_button
            .connect_clicked(move |_| add_profile_session(&state, Profile::Shell));
    }
    {
        let state = state.clone();
        workspace_widgets
            .launch_buttons
            .codex_button
            .connect_clicked(move |_| add_profile_session(&state, Profile::Codex));
    }
    {
        let state = state.clone();
        workspace_widgets
            .launch_buttons
            .claude_button
            .connect_clicked(move |_| add_profile_session(&state, Profile::Claude));
    }
    {
        let state = state.clone();
        workspace_widgets
            .launch_buttons
            .mistral_button
            .connect_clicked(move |_| add_profile_session(&state, Profile::Mistral));
    }
    {
        let state = state.clone();
        workspace_widgets
            .launch_buttons
            .custom_button
            .connect_clicked(move |_| prompt_custom_session(&state, None));
    }
    {
        let state = state.clone();
        sidebar_widgets
            .refresh_button
            .connect_clicked(move |_| refresh_projects(&state));
    }
    {
        let state = state.clone();
        let sidebar_toggle_button = state.borrow().sidebar_toggle_button.clone();
        sidebar_toggle_button.connect_clicked(move |_| toggle_sidebar(&state));
    }
    {
        let state = state.clone();
        let root_paned = state.borrow().root_paned.clone();
        root_paned.connect_position_notify(move |paned| {
            let position = paned.position();
            if position > 0 {
                let mut state_mut = state.borrow_mut();
                if state_mut.sidebar_visible {
                    state_mut.last_sidebar_width = position;
                }
            }
        });
    }
    {
        let state = state.clone();
        let reset_button = state.borrow().reset_button.clone();
        reset_button.connect_clicked(move |_| reset_selected_session(&state));
    }
    {
        let state = state.clone();
        let close_button = state.borrow().close_button.clone();
        close_button.connect_clicked(move |_| close_selected_session(&state));
    }
    {
        let state = state.clone();
        let zoom_button = state.borrow().zoom_button.clone();
        zoom_button.connect_clicked(move |_| toggle_zoom_selected(&state));
    }
    {
        let state = state.clone();
        let repo_scope_combo = state.borrow().repo_scope_combo.clone();
        repo_scope_combo.connect_selected_notify(move |_| update_project_ui(&state));
    }
    {
        let state = state.clone();
        let fetch_button = state.borrow().fetch_button.clone();
        fetch_button.connect_clicked(move |_| run_repo_action_for_scope(&state, RepoAction::Fetch));
    }
    {
        let state = state.clone();
        let pull_button = state.borrow().pull_button.clone();
        pull_button.connect_clicked(move |_| run_repo_action_for_scope(&state, RepoAction::Pull));
    }
    {
        let state = state.clone();
        let commit_push_button = state.borrow().commit_push_button.clone();
        commit_push_button.connect_clicked(move |_| prompt_commit_and_push(&state));
    }
    {
        let state = state.clone();
        application.connect_shutdown(move |_| {
            kill_all_sessions(&state);
            let _ = persist_sessions(&state);
        });
    }

    update_sidebar_button(&state);
    refresh_projects(&state);

    update_layout(&state);
    window.present();
    Ok(())
}

fn build_sidebar() -> (GtkBox, SidebarWidgets) {
    let sidebar = GtkBox::new(Orientation::Vertical, 8);
    sidebar.add_css_class("sidebar");
    sidebar.set_width_request(250);

    let header = GtkBox::new(Orientation::Vertical, 6);
    header.add_css_class("sidebar-header");

    let title_row = GtkBox::new(Orientation::Horizontal, 8);

    let title = Label::new(Some("Explorer"));
    title.add_css_class("sidebar-title");
    title.set_xalign(0.0);
    title.set_hexpand(true);

    let project_count_label = Label::new(Some("0 repos"));
    project_count_label.add_css_class("count-label");

    let refresh_button = action_button("Refresh");
    refresh_button.set_tooltip_text(Some("Rescan the configured project roots."));
    refresh_button.set_hexpand(true);

    let repo_scope_combo = DropDown::from_strings(&["Selected", "All"]);
    repo_scope_combo.set_selected(0);
    repo_scope_combo.set_hexpand(true);
    repo_scope_combo.set_tooltip_text(Some(
        "Choose whether repo actions target the selected repo or every repo.",
    ));

    let scope_row = GtkBox::new(Orientation::Horizontal, 6);
    scope_row.add_css_class("sidebar-toolbar-row");
    scope_row.append(&refresh_button);
    scope_row.append(&repo_scope_combo);

    let fetch_button = action_button("Fetch");
    fetch_button.set_tooltip_text(Some("Fetch remote changes for the chosen repo scope."));
    let pull_button = action_button("Pull");
    pull_button.set_tooltip_text(Some(
        "Fetch, then fast-forward pull for the chosen repo scope when each repo is safe to update.",
    ));
    let commit_push_button = action_button("Commit+Push");
    commit_push_button.add_css_class("primary-button");
    commit_push_button.set_tooltip_text(Some(
        "Commit pending changes, or push existing local commits, in the selected repo.",
    ));

    let git_row = GtkBox::new(Orientation::Horizontal, 6);
    git_row.add_css_class("sidebar-toolbar-row");
    git_row.append(&fetch_button);
    git_row.append(&pull_button);
    git_row.append(&commit_push_button);

    title_row.append(&title);
    title_row.append(&project_count_label);

    let hint = Label::new(Some(
        "Click a repo to toggle selection. Double-click to open a shell there.",
    ));
    hint.add_css_class("hint-label");
    hint.set_wrap(true);
    hint.set_xalign(0.0);

    header.append(&title_row);
    header.append(&scope_row);
    header.append(&git_row);

    let project_list = ListBox::new();
    project_list.add_css_class("project-list");
    project_list.set_selection_mode(gtk::SelectionMode::None);
    project_list.set_vexpand(true);

    let scroller = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&project_list)
        .build();
    scroller.add_css_class("project-scroller");

    sidebar.append(&header);
    sidebar.append(&hint);
    sidebar.append(&scroller);

    (
        sidebar,
        SidebarWidgets {
            project_list,
            project_count_label,
            refresh_button,
            repo_scope_combo,
            fetch_button,
            pull_button,
            commit_push_button,
        },
    )
}

fn build_workspace() -> (GtkBox, WorkspaceWidgets) {
    let workspace = GtkBox::new(Orientation::Vertical, 8);
    workspace.add_css_class("workspace");
    workspace.set_hexpand(true);
    workspace.set_vexpand(true);

    let header = GtkBox::new(Orientation::Vertical, 8);
    header.add_css_class("workspace-header");

    let title_row = GtkBox::new(Orientation::Horizontal, 8);
    let sidebar_toggle_button = action_button("<");
    sidebar_toggle_button.set_tooltip_text(Some("Collapse or expand the repo sidebar."));

    let meta = GtkBox::new(Orientation::Vertical, 2);
    meta.set_hexpand(true);

    let workspace_title_label = Label::new(Some("No repository selected"));
    workspace_title_label.add_css_class("workspace-title");
    workspace_title_label.set_xalign(0.0);

    let workspace_path_label = Label::new(Some(
        "Select a repository on the left, then start a terminal session.",
    ));
    workspace_path_label.add_css_class("workspace-path");
    workspace_path_label.set_xalign(0.0);
    workspace_path_label.set_ellipsize(EllipsizeMode::Middle);

    let workspace_repo_status_label =
        Label::new(Some("Select a repository to inspect its git state."));
    workspace_repo_status_label.add_css_class("workspace-repo-status");
    workspace_repo_status_label.set_xalign(0.0);
    workspace_repo_status_label.set_wrap(true);

    meta.append(&workspace_title_label);
    meta.append(&workspace_path_label);
    meta.append(&workspace_repo_status_label);

    title_row.append(&sidebar_toggle_button);
    title_row.append(&meta);

    let toolbar_row = GtkBox::new(Orientation::Horizontal, 8);

    let launch_group = GtkBox::new(Orientation::Horizontal, 8);
    launch_group.add_css_class("toolbar-group");

    let shell_button = action_button("Shell");
    shell_button.set_tooltip_text(Some("Open a shell in the selected repository."));
    let codex_button = action_button("Codex");
    codex_button.set_tooltip_text(Some("Launch Codex in the selected repository."));
    let claude_button = action_button("Claude");
    claude_button.set_tooltip_text(Some("Launch Claude in the selected repository."));
    let mistral_button = action_button("Mistral");
    mistral_button.set_tooltip_text(Some("Launch Mistral in the selected repository."));
    let custom_button = action_button("Custom");
    custom_button.set_tooltip_text(Some("Launch a custom command in the selected repository."));

    codex_button.set_sensitive(binary_exists("codex"));
    claude_button.set_sensitive(binary_exists("claude"));
    mistral_button.set_sensitive(binary_exists("mistral"));

    launch_group.append(&shell_button);
    launch_group.append(&codex_button);
    launch_group.append(&claude_button);
    launch_group.append(&mistral_button);
    launch_group.append(&custom_button);

    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);

    let pane_group = GtkBox::new(Orientation::Horizontal, 8);
    pane_group.add_css_class("toolbar-group");

    let reset_button = action_button("Reset");
    reset_button.set_tooltip_text(Some(
        "Kill and relaunch the focused pane from scratch with the same command.",
    ));
    let zoom_button = action_button("Zoom");
    let close_button = action_button("Close");
    pane_group.append(&reset_button);
    pane_group.append(&zoom_button);
    pane_group.append(&close_button);

    toolbar_row.append(&launch_group);
    toolbar_row.append(&spacer);
    toolbar_row.append(&pane_group);

    header.append(&title_row);
    header.append(&toolbar_row);

    let grid = Grid::builder()
        .column_spacing(8)
        .row_spacing(8)
        .column_homogeneous(true)
        .row_homogeneous(true)
        .hexpand(true)
        .vexpand(true)
        .build();

    let content_scroller = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&grid)
        .build();
    content_scroller.add_css_class("stage-scroller");

    let empty_state = build_empty_state();
    let stack = Stack::builder().hexpand(true).vexpand(true).build();
    stack.add_named(&empty_state, Some("empty"));
    stack.add_named(&content_scroller, Some("grid"));
    stack.set_visible_child_name("empty");

    let stage = GtkBox::new(Orientation::Vertical, 0);
    stage.add_css_class("workspace-stage");
    stage.set_hexpand(true);
    stage.set_vexpand(true);
    stage.append(&stack);

    let footer = build_footer();

    workspace.append(&header);
    workspace.append(&stage);
    workspace.append(&footer.2);

    (
        workspace,
        WorkspaceWidgets {
            stack,
            grid,
            sidebar_toggle_button,
            workspace_title_label,
            workspace_path_label,
            workspace_repo_status_label,
            status_label: footer.0,
            count_label: footer.1,
            reset_button,
            launch_buttons: LaunchButtons {
                shell_button,
                codex_button,
                claude_button,
                mistral_button,
                custom_button,
            },
            close_button,
            zoom_button,
        },
    )
}

fn build_empty_state() -> GtkBox {
    let box_ = GtkBox::new(Orientation::Vertical, 8);
    box_.add_css_class("empty-state");
    box_.set_valign(Align::Center);
    box_.set_halign(Align::Center);

    let title = Label::new(Some("No terminals running"));
    title.add_css_class("empty-title");

    let body = Label::new(Some(
        "Select a repository on the left, then use the buttons above to open Shell, Codex, Claude, Mistral, or a custom session.",
    ));
    body.add_css_class("empty-body");
    body.set_wrap(true);
    body.set_justify(gtk::Justification::Center);

    box_.append(&title);
    box_.append(&body);
    box_
}

fn build_footer() -> (Label, Label, GtkBox) {
    let footer = GtkBox::new(Orientation::Horizontal, 8);
    footer.add_css_class("footer");

    let status_label = Label::new(Some("Ready."));
    status_label.add_css_class("footer-status");
    status_label.set_xalign(0.0);
    status_label.set_hexpand(true);

    let count_label = Label::new(Some("0 panes"));
    count_label.add_css_class("footer-count");
    count_label.set_xalign(1.0);

    footer.append(&status_label);
    footer.append(&count_label);
    (status_label, count_label, footer)
}

fn action_button(label: &str) -> Button {
    let button = Button::with_label(label);
    button.add_css_class("action-button");
    button
}

fn refresh_projects(state: &SharedState) {
    let (previous_path, roots) = {
        let state = state.borrow();
        (
            selected_project_ref(&state).map(|project| project.path.to_string_lossy().to_string()),
            state.project_roots.clone(),
        )
    };

    let mut projects = discover_projects(&roots);
    if projects.is_empty() {
        if let Ok(cwd) = env::current_dir() {
            projects.push(ProjectInfo {
                name: cwd
                    .file_name()
                    .and_then(|part| part.to_str())
                    .unwrap_or("current")
                    .to_string(),
                repo_status: inspect_project(&cwd),
                path: cwd,
            });
        }
    }

    for project in &mut projects {
        project.repo_status = inspect_project(&project.path);
    }

    let refresh_summary = describe_project_overview(&projects);

    {
        let mut state_mut = state.borrow_mut();
        state_mut.projects = projects;
        state_mut.selected_project_index = previous_path.and_then(|path| {
            state_mut
                .projects
                .iter()
                .position(|project| project.path.to_string_lossy() == path)
        });
    }

    rebuild_project_list(state);
    push_status(state, refresh_summary);
}

fn rebuild_project_list(state: &SharedState) {
    let (project_list, projects_for_rows, selected_index) = {
        let state = state.borrow();
        (
            state.project_list.clone(),
            state.projects.clone(),
            state.selected_project_index,
        )
    };

    while let Some(child) = project_list.first_child() {
        project_list.remove(&child);
    }

    for (index, project) in projects_for_rows.iter().enumerate() {
        let row = build_project_row(state, index, project);
        project_list.append(&row);
    }

    if let Some(index) = selected_index {
        select_project_row(state, index);
    } else {
        update_project_ui(state);
    }
}

fn build_project_row(state: &SharedState, index: usize, project: &ProjectInfo) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("project-row");
    row.set_tooltip_text(Some(&format!(
        "{}\nGit: {}",
        project.path.display(),
        project.repo_status.short_label()
    )));

    let body = GtkBox::new(Orientation::Vertical, 2);
    body.add_css_class("project-row-body");
    body.set_hexpand(true);

    let title_row = GtkBox::new(Orientation::Horizontal, 8);

    let name = Label::new(Some(&project.name));
    name.add_css_class("project-name");
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(EllipsizeMode::End);

    let repo_status = Label::new(Some(&project.repo_status.short_label()));
    repo_status.add_css_class("repo-status-label");
    repo_status.add_css_class(project.repo_status.css_class());
    repo_status.set_xalign(1.0);

    title_row.append(&name);
    title_row.append(&repo_status);

    let path = Label::new(Some(&project.path.display().to_string()));
    path.add_css_class("project-path");
    path.set_xalign(0.0);
    path.set_ellipsize(EllipsizeMode::Middle);

    body.append(&title_row);
    body.append(&path);

    let click = gtk::GestureClick::new();
    click.set_button(1);
    {
        let state = state.clone();
        click.connect_released(move |_, presses, _, _| match presses {
            2 => {
                select_project_row(&state, index);
                if let Some(project) = project_at_index(&state, index) {
                    add_profile_session_in_cwd(&state, Profile::Shell, project.path);
                }
            }
            1 => toggle_project_selection(&state, index),
            _ => {}
        });
    }
    row.add_controller(click);
    row.set_child(Some(&body));
    row
}

fn update_project_ui(state: &SharedState) {
    let (
        project_list,
        project_count_label,
        workspace_title_label,
        workspace_path_label,
        workspace_repo_status_label,
        fetch_button,
        pull_button,
        commit_push_button,
        selected_index,
        project_total,
        action_target_count,
        commit_push_enabled,
        selected_title,
        selected_path,
        selected_status,
    ) = {
        let state = state.borrow();
        let selected = selected_project_ref(&state);
        (
            state.project_list.clone(),
            state.project_count_label.clone(),
            state.workspace_title_label.clone(),
            state.workspace_path_label.clone(),
            state.workspace_repo_status_label.clone(),
            state.fetch_button.clone(),
            state.pull_button.clone(),
            state.commit_push_button.clone(),
            state.selected_project_index,
            state.projects.len(),
            repo_action_targets_from_state(&state).len(),
            commit_push_mode_from_state(&state).is_some(),
            selected
                .map(|project| project.name.clone())
                .unwrap_or_else(|| "No repository selected".to_string()),
            selected
                .map(|project| project.path.display().to_string())
                .unwrap_or_else(|| {
                    "Select a repository on the left, then start a terminal session.".to_string()
                }),
            selected
                .map(|project| format!("Git: {}", project.repo_status.short_label()))
                .unwrap_or_else(|| "Select a repository to inspect its git state.".to_string()),
        )
    };

    project_count_label.set_text(&describe_project_count(project_total));
    workspace_title_label.set_text(&selected_title);
    workspace_path_label.set_text(&selected_path);
    workspace_repo_status_label.set_text(&selected_status);
    fetch_button.set_sensitive(action_target_count > 0);
    pull_button.set_sensitive(action_target_count > 0);
    commit_push_button.set_sensitive(commit_push_enabled);
    update_project_row_classes(&project_list, selected_index);
}

fn describe_project_count(project_total: usize) -> String {
    format!(
        "{project_total} {}",
        if project_total == 1 { "repo" } else { "repos" }
    )
}

fn describe_project_overview(projects: &[ProjectInfo]) -> String {
    let dirty = projects
        .iter()
        .filter(|project| project.repo_status.dirty)
        .count();
    let behind = projects
        .iter()
        .filter(|project| project.repo_status.behind > 0)
        .count();
    let ahead = projects
        .iter()
        .filter(|project| project.repo_status.ahead > 0)
        .count();
    let attention = projects
        .iter()
        .filter(|project| project.repo_status.needs_attention())
        .count();

    let mut parts = Vec::new();
    if dirty > 0 {
        parts.push(format!("{dirty} dirty"));
    }
    if behind > 0 {
        parts.push(format!("{behind} behind"));
    }
    if ahead > 0 {
        parts.push(format!("{ahead} ahead"));
    }
    if attention > 0 && parts.is_empty() {
        parts.push(format!("{attention} need attention"));
    }

    if parts.is_empty() {
        format!("loaded {} repositories", projects.len())
    } else {
        format!(
            "loaded {} repositories ({})",
            projects.len(),
            parts.join(", ")
        )
    }
}

fn update_project_row_classes(project_list: &ListBox, selected_index: Option<usize>) {
    let mut index = 0;
    while let Some(row) = project_list.row_at_index(index) {
        if selected_index == usize::try_from(index).ok() {
            row.add_css_class("selected-project-row");
        } else {
            row.remove_css_class("selected-project-row");
        }
        index += 1;
    }
}

fn select_project_row(state: &SharedState, index: usize) {
    set_selected_project_index(state, Some(index));
}

fn set_selected_project_index(state: &SharedState, index: Option<usize>) {
    {
        let mut state_mut = state.borrow_mut();
        state_mut.selected_project_index = index.filter(|index| *index < state_mut.projects.len());
    }
    update_project_ui(state);
}

fn toggle_project_selection(state: &SharedState, index: usize) {
    let new_index = {
        let state_ref = state.borrow();
        if index >= state_ref.projects.len() {
            return;
        }

        match state_ref.selected_project_index {
            Some(selected_index) if selected_index == index => None,
            _ => Some(index),
        }
    };

    set_selected_project_index(state, new_index);

    if let Some(project) = project_at_index(state, index) {
        let message = if new_index.is_some() {
            format!("selected repo: {}", project.path.display())
        } else {
            format!("cleared repo selection: {}", project.path.display())
        };
        push_status(state, message);
    }
}

fn current_repo_action_scope(state: &AppState) -> RepoActionScope {
    RepoActionScope::from_index(state.repo_scope_combo.selected())
}

fn repo_action_targets_from_state(state: &AppState) -> Vec<ProjectInfo> {
    match current_repo_action_scope(state) {
        RepoActionScope::Selected => selected_project_ref(state).cloned().into_iter().collect(),
        RepoActionScope::All => state.projects.clone(),
    }
}

fn repo_action_targets(state: &SharedState) -> (RepoActionScope, Vec<ProjectInfo>) {
    let state = state.borrow();
    (
        current_repo_action_scope(&state),
        repo_action_targets_from_state(&state),
    )
}

fn toggle_sidebar(state: &SharedState) {
    let visible = {
        let mut state_mut = state.borrow_mut();
        if state_mut.sidebar_visible {
            let current_position = state_mut.root_paned.position();
            if current_position > 0 {
                state_mut.last_sidebar_width = current_position;
            }
            state_mut
                .root_paned
                .set_start_child(Option::<&GtkBox>::None);
            state_mut.sidebar_visible = false;
            false
        } else {
            let sidebar = state_mut.sidebar.clone();
            state_mut.root_paned.set_start_child(Some(&sidebar));
            state_mut
                .root_paned
                .set_position(state_mut.last_sidebar_width.max(180));
            state_mut.sidebar_visible = true;
            true
        }
    };
    update_sidebar_button(state);
    push_status(
        state,
        if visible {
            "sidebar shown".to_string()
        } else {
            "sidebar hidden".to_string()
        },
    );
}

fn update_sidebar_button(state: &SharedState) {
    let (button, visible) = {
        let state = state.borrow();
        (state.sidebar_toggle_button.clone(), state.sidebar_visible)
    };
    button.set_label(if visible { "<" } else { ">" });
}

fn add_profile_session(state: &SharedState, profile: Profile) {
    let cwd = selected_project_path(state)
        .or_else(|| env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    add_profile_session_in_cwd(state, profile, cwd);
}

fn add_profile_session_in_cwd(state: &SharedState, profile: Profile, cwd: PathBuf) {
    let spec = SessionSpec {
        cwd,
        command: profile.default_command(),
        name: None,
        profile,
    };

    if let Err(error) = spawn_session(state, spec) {
        show_output_dialog(
            &state.borrow().window,
            "Launch Failed",
            &format!("{error:#}"),
        );
    }
}

fn install_terminal_interactions(state: &SharedState, session: &Rc<SessionView>) {
    let click_state = Rc::new(TerminalClickState::default());
    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);

    {
        let state = state.clone();
        let session = session.clone();
        let click_state = click_state.clone();
        click.connect_pressed(move |_, presses, x, y| {
            if presses != 1 {
                click_state.clear_selection_on_release.set(false);
                return;
            }

            click_state.press_x.set(x);
            click_state.press_y.set(y);
            click_state
                .clear_selection_on_release
                .set(!session.terminal.has_focus());

            select_session(&state, session.id);
            session.terminal.grab_focus();
        });
    }

    {
        let session = session.clone();
        let click_state = click_state.clone();
        click.connect_released(move |_, presses, x, y| {
            if presses != 1 {
                click_state.clear_selection_on_release.set(false);
                return;
            }

            let should_clear = click_state.clear_selection_on_release.replace(false);
            if !should_clear {
                return;
            }

            let dx = x - click_state.press_x.get();
            let dy = y - click_state.press_y.get();
            if dx.hypot(dy) <= 4.0 {
                session.terminal.unselect_all();
            }
        });
    }

    session.terminal.add_controller(click);
}

fn focus_session_terminal(session: &Rc<SessionView>) {
    let terminal = session.terminal.clone();
    glib::idle_add_local_once(move || {
        terminal.grab_focus();
    });
}

fn focus_selected_session_terminal(state: &SharedState) {
    if let Some(session) = selected_session(state) {
        focus_session_terminal(&session);
    }
}

#[allow(deprecated)]
fn prompt_custom_session(state: &SharedState, cwd_override: Option<PathBuf>) {
    let window = state.borrow().window.clone();
    let dialog = gtk::Dialog::builder()
        .title("Custom Terminal")
        .transient_for(&window)
        .modal(true)
        .build();

    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("Launch", ResponseType::Accept);

    let content = dialog.content_area();
    content.set_spacing(10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let title_entry = Entry::builder()
        .placeholder_text("Optional pane title")
        .build();
    let command_entry = Entry::builder()
        .placeholder_text("Command, for example: codex --model gpt-5.4")
        .build();

    content.append(&title_entry);
    content.append(&command_entry);

    {
        let state = state.clone();
        let title_entry = title_entry.clone();
        let command_entry = command_entry.clone();
        let cwd_override = cwd_override.clone();
        dialog.connect_response(move |dialog, response| {
            if response == ResponseType::Accept {
                let command = command_entry.text().trim().to_string();
                if command.is_empty() {
                    show_output_dialog(
                        &state.borrow().window,
                        "Missing Command",
                        "Enter a command for the custom terminal.",
                    );
                } else {
                    let cwd = cwd_override
                        .clone()
                        .or_else(|| selected_project_path(&state))
                        .or_else(|| env::current_dir().ok())
                        .unwrap_or_else(|| PathBuf::from("."));
                    let name = {
                        let value = title_entry.text().trim().to_string();
                        (!value.is_empty()).then_some(value)
                    };
                    let spec = SessionSpec {
                        cwd,
                        command,
                        name,
                        profile: Profile::Custom,
                    };
                    if let Err(error) = spawn_session(&state, spec) {
                        show_output_dialog(
                            &state.borrow().window,
                            "Launch Failed",
                            &format!("{error:#}"),
                        );
                    }
                }
            }
            dialog.close();
        });
    }

    dialog.present();
}

#[allow(deprecated)]
fn prompt_commit_and_push(state: &SharedState) {
    let (target, mode) = {
        let state = state.borrow();
        let Some(target) = commit_push_target_from_state(&state) else {
            return;
        };
        let Some(mode) = commit_push_mode_from_state(&state) else {
            return;
        };
        (target, mode)
    };

    if matches!(mode, CommitPushMode::PushOnly) {
        run_commit_and_push(state, target, mode, None);
        return;
    }

    let window = state.borrow().window.clone();
    let dialog = gtk::Dialog::builder()
        .title(mode.dialog_title())
        .transient_for(&window)
        .modal(true)
        .build();
    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("Commit+Push", ResponseType::Accept);

    let content = dialog.content_area();
    content.set_spacing(10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let target_label = Label::new(Some(&format!("Target: {}", target.cwd.display())));
    target_label.add_css_class("hint-label");
    target_label.set_wrap(true);
    target_label.set_xalign(0.0);
    content.append(&target_label);

    let entry = Entry::builder().placeholder_text("Commit message").build();
    content.append(&entry);

    {
        let state = state.clone();
        let entry = entry.clone();
        let target = target.clone();
        dialog.connect_response(move |dialog, response| {
            if response == ResponseType::Accept {
                let message = entry.text().trim().to_string();
                if message.is_empty() {
                    show_output_dialog(
                        &state.borrow().window,
                        "Missing Commit Message",
                        "Enter a commit message before continuing.",
                    );
                } else {
                    run_commit_and_push(
                        &state,
                        target.clone(),
                        CommitPushMode::CommitAndPush,
                        Some(message),
                    );
                }
            }
            dialog.close();
        });
    }

    dialog.present();
}

fn spawn_session(state: &SharedState, spec: SessionSpec) -> Result<()> {
    let spec = spec.normalized()?;
    let id = next_session_id(state);
    let title = spec.title();
    let subtitle = spec.subtitle();

    let terminal = Terminal::new();
    terminal.set_hexpand(true);
    terminal.set_vexpand(true);
    terminal.set_cursor_blink_mode(CursorBlinkMode::On);
    terminal.set_focus_on_click(true);
    terminal.set_focusable(true);
    terminal.set_input_enabled(true);
    terminal.set_scrollback_lines(20_000);
    terminal.set_mouse_autohide(true);
    terminal.set_size_request(360, 220);
    apply_terminal_theme(&terminal);

    let card = GtkBox::new(Orientation::Vertical, 0);
    card.add_css_class("session-card");

    let title_label = Label::new(Some(&title));
    title_label.add_css_class("session-title");
    title_label.set_xalign(0.0);

    let subtitle_label = Label::new(Some(&subtitle));
    subtitle_label.add_css_class("session-subtitle");
    subtitle_label.set_xalign(0.0);
    subtitle_label.set_wrap(true);
    subtitle_label.set_ellipsize(EllipsizeMode::Middle);

    let badge_label = Label::new(Some(spec.profile.label()));
    badge_label.add_css_class("profile-badge");
    badge_label.add_css_class(profile_css_class(spec.profile));

    let status_label = Label::new(Some("LIVE"));
    status_label.add_css_class("live-pill");

    let meta_box = GtkBox::new(Orientation::Vertical, 2);
    meta_box.append(&title_label);
    meta_box.append(&subtitle_label);

    let badge_box = GtkBox::new(Orientation::Horizontal, 6);
    badge_box.append(&badge_label);
    badge_box.append(&status_label);

    let header = CenterBox::new();
    header.add_css_class("session-header");
    header.set_start_widget(Some(&meta_box));
    header.set_end_widget(Some(&badge_box));

    card.append(&header);
    card.append(&terminal);

    let session = Rc::new(SessionView {
        id,
        spec: RefCell::new(spec.clone()),
        card: card.clone(),
        subtitle_label,
        status_label,
        terminal: terminal.clone(),
        pid: Cell::new(None),
        alive: Cell::new(true),
    });

    let click = gtk::GestureClick::new();
    {
        let state = state.clone();
        let session = session.clone();
        click.connect_pressed(move |_, presses, _, _| {
            select_session(&state, session.id);
            session.terminal.grab_focus();
            if presses == 2 {
                toggle_zoom_for_id(&state, session.id);
            }
        });
    }
    header.add_controller(click);

    install_terminal_interactions(state, &session);

    {
        let state = state.clone();
        let session = session.clone();
        terminal.connect_has_focus_notify(move |terminal| {
            if terminal.has_focus() {
                select_session(&state, session.id);
            }
        });
    }

    {
        let state = state.clone();
        let session = session.clone();
        terminal.connect_current_directory_uri_changed(move |term| {
            if let Some(uri) = term.current_directory_uri() {
                if let Some(path) = gio::File::for_uri(&uri).path() {
                    session.spec.borrow_mut().cwd = path.clone();
                    session
                        .subtitle_label
                        .set_text(&session.spec.borrow().subtitle());
                    let _ = persist_sessions(&state);
                }
            }
        });
    }

    {
        let state = state.clone();
        let session = session.clone();
        terminal.connect_child_exited(move |_, exit_status| {
            session.alive.set(false);
            session.pid.set(None);
            session
                .status_label
                .set_text(&format!("EXIT {exit_status}"));
            session.status_label.remove_css_class("live-pill");
            session.status_label.add_css_class("dead-pill");
            push_status(
                &state,
                format!(
                    "{} exited with status {exit_status}",
                    session.spec.borrow().title()
                ),
            );
        });
    }

    {
        let mut state_mut = state.borrow_mut();
        state_mut.sessions.push(session.clone());
        state_mut.selected_session_id = Some(id);
    }

    persist_sessions(state)?;
    update_layout(state);
    focus_session_terminal(&session);
    launch_terminal_process(state, &session)?;
    push_status(
        state,
        format!(
            "launched {} in {}",
            spec.profile.label(),
            spec.cwd.display()
        ),
    );
    Ok(())
}

fn launch_terminal_process(state: &SharedState, session: &Rc<SessionView>) -> Result<()> {
    let spec = session.spec.borrow().clone();
    let argv = spawn_argv(&spec)?;
    let argv_strings = argv.clone();
    let workdir = spec.cwd.to_string_lossy().to_string();
    let envv = spawn_environment();

    let terminal = session.terminal.clone();
    let session = session.clone();
    let state = state.clone();
    glib::MainContext::default().spawn_local(async move {
        let argv_refs: Vec<&str> = argv_strings.iter().map(String::as_str).collect();
        let env_refs: Vec<&str> = envv.iter().map(String::as_str).collect();
        let result = terminal
            .spawn_future(
                PtyFlags::DEFAULT,
                Some(&workdir),
                &argv_refs,
                &env_refs,
                glib::SpawnFlags::DEFAULT,
                || {},
                -1,
            )
            .await;

        match result {
            Ok(pid) => {
                session.pid.set(Some(pid));
                push_status(&state, format!("{} ready", session.spec.borrow().title()));
            }
            Err(error) => {
                session.alive.set(false);
                session.status_label.set_text("FAILED");
                session.status_label.remove_css_class("live-pill");
                session.status_label.add_css_class("dead-pill");
                show_output_dialog(
                    &state.borrow().window,
                    "Spawn Failed",
                    &format!(
                        "Could not launch `{}` in `{}`.\n\n{}",
                        spec.resolved_command(),
                        spec.cwd.display(),
                        error
                    ),
                );
            }
        }
    });
    Ok(())
}

fn update_layout(state: &SharedState) {
    let state_mut = state.borrow_mut();

    while let Some(child) = state_mut.grid.first_child() {
        state_mut.grid.remove(&child);
    }

    let visible_sessions: Vec<Rc<SessionView>> = if let Some(id) = state_mut.zoomed_session_id {
        state_mut
            .sessions
            .iter()
            .filter(|session| session.id == id)
            .cloned()
            .collect()
    } else {
        state_mut.sessions.clone()
    };

    if visible_sessions.is_empty() {
        state_mut.stack.set_visible_child_name("empty");
    } else {
        state_mut.stack.set_visible_child_name("grid");
        attach_sessions(&state_mut.grid, &visible_sessions);
    }

    for session in &state_mut.sessions {
        if state_mut.selected_session_id == Some(session.id) {
            session.card.add_css_class("selected-card");
        } else {
            session.card.remove_css_class("selected-card");
        }
    }

    let total = state_mut.sessions.len();
    state_mut.count_label.set_text(&format!(
        "{total} {}",
        if total == 1 { "pane" } else { "panes" }
    ));
    let focused = state_mut.selected_session_id.is_some();
    state_mut.reset_button.set_sensitive(focused);
    state_mut.close_button.set_sensitive(focused);
    state_mut.zoom_button.set_sensitive(focused);
    state_mut
        .commit_push_button
        .set_sensitive(commit_push_mode_from_state(&state_mut).is_some());
}

fn attach_sessions(grid: &Grid, sessions: &[Rc<SessionView>]) {
    match sessions.len() {
        0 => {}
        1 => grid.attach(&sessions[0].card, 0, 0, 1, 1),
        2 => {
            grid.attach(&sessions[0].card, 0, 0, 1, 1);
            grid.attach(&sessions[1].card, 1, 0, 1, 1);
        }
        3 => {
            grid.attach(&sessions[0].card, 0, 0, 1, 1);
            grid.attach(&sessions[1].card, 1, 0, 1, 1);
            grid.attach(&sessions[2].card, 0, 1, 2, 1);
        }
        4 => {
            for (index, session) in sessions.iter().enumerate() {
                let row = (index / 2) as i32;
                let col = (index % 2) as i32;
                grid.attach(&session.card, col, row, 1, 1);
            }
        }
        5 | 6 => {
            for (index, session) in sessions.iter().enumerate() {
                let row = (index / 3) as i32;
                let col = (index % 3) as i32;
                grid.attach(&session.card, col, row, 1, 1);
            }
        }
        _ => {
            let columns = 4usize;
            for (index, session) in sessions.iter().enumerate() {
                let row = (index / columns) as i32;
                let col = (index % columns) as i32;
                grid.attach(&session.card, col, row, 1, 1);
            }
        }
    }
}

fn select_session(state: &SharedState, id: u64) {
    state.borrow_mut().selected_session_id = Some(id);
    update_layout(state);
}

fn remove_session_by_id(state: &SharedState, id: u64) -> Option<Rc<SessionView>> {
    let session = {
        let mut state_mut = state.borrow_mut();
        let index = state_mut
            .sessions
            .iter()
            .position(|session| session.id == id)?;

        let session = state_mut.sessions.remove(index);
        if session.alive.get() {
            kill_pid(session.pid.get());
        }

        if state_mut.selected_session_id == Some(id) {
            state_mut.selected_session_id = state_mut.sessions.last().map(|session| session.id);
        }
        if state_mut.zoomed_session_id == Some(id) {
            state_mut.zoomed_session_id = None;
        }
        session
    };

    Some(session)
}

fn close_selected_session(state: &SharedState) {
    let Some(id) = state.borrow().selected_session_id else {
        return;
    };

    let Some(removed) = remove_session_by_id(state, id) else {
        return;
    };

    removed.card.unparent();
    if let Err(error) = persist_sessions(state) {
        show_output_dialog(
            &state.borrow().window,
            "Persistence Error",
            &format!("{error:#}"),
        );
    }
    update_layout(state);
    focus_selected_session_terminal(state);
    push_status(state, format!("closed {}", removed.spec.borrow().title()));
}

fn reset_selected_session(state: &SharedState) {
    let Some(session) = selected_session(state) else {
        return;
    };

    let id = session.id;
    let spec = session.spec.borrow().clone();
    let title = spec.title();
    let was_zoomed = state.borrow().zoomed_session_id == Some(id);

    let Some(removed) = remove_session_by_id(state, id) else {
        return;
    };

    removed.card.unparent();
    if let Err(error) = persist_sessions(state) {
        show_output_dialog(
            &state.borrow().window,
            "Persistence Error",
            &format!("{error:#}"),
        );
    }

    update_layout(state);

    if let Err(error) = spawn_session(state, spec) {
        show_output_dialog(
            &state.borrow().window,
            "Reset Failed",
            &format!("{error:#}"),
        );
        return;
    }

    if was_zoomed {
        let new_id = state.borrow().selected_session_id;
        if let Some(new_id) = new_id {
            state.borrow_mut().zoomed_session_id = Some(new_id);
            update_layout(state);
        }
    }

    push_status(state, format!("reset {}", title));
}

fn toggle_zoom_selected(state: &SharedState) {
    let selected = state.borrow().selected_session_id;
    if let Some(id) = selected {
        toggle_zoom_for_id(state, id);
    }
}

fn toggle_zoom_for_id(state: &SharedState, id: u64) {
    let message = {
        let mut state_mut = state.borrow_mut();
        if state_mut.zoomed_session_id == Some(id) {
            state_mut.zoomed_session_id = None;
            "zoom released".to_string()
        } else {
            state_mut.zoomed_session_id = Some(id);
            "zoomed focused pane".to_string()
        }
    };
    update_layout(state);
    push_status(state, message);
}

fn run_commit_and_push(
    state: &SharedState,
    target: CommitPushTarget,
    mode: CommitPushMode,
    message: Option<String>,
) {
    let result = execute_commit_and_push(&target.cwd, message.as_deref());
    refresh_project_status_for_cwd(state, &target.cwd);

    match result {
        Ok(output) => {
            show_output_dialog(
                &state.borrow().window,
                &format!("{} for {}", mode.report_title(), target.title),
                &output,
            );
            push_status(
                state,
                format!(
                    "{} finished in {}",
                    mode.report_title().to_lowercase(),
                    target.cwd.display()
                ),
            );
        }
        Err(error) => {
            show_output_dialog(
                &state.borrow().window,
                &format!("{} Failed", mode.report_title()),
                &format!("{error:#}"),
            );
        }
    }
}

fn run_repo_action_for_scope(state: &SharedState, action: RepoAction) {
    let (scope, targets) = repo_action_targets(state);
    if targets.is_empty() {
        return;
    }

    let reports = targets
        .iter()
        .map(|project| execute_repo_action(action, project))
        .collect::<Vec<_>>();

    apply_repo_action_reports(state, &reports);

    let summary = summarize_repo_action(action, scope, &reports);
    let details = render_repo_action_report(action, &reports);
    push_status_with_details(state, summary, Some(details));
}

fn execute_commit_and_push(cwd: &Path, message: Option<&str>) -> Result<String> {
    let mut transcript = Vec::new();

    if let Some(message) = message {
        transcript.push(format_git_step(
            "git add -A",
            run_git_command(cwd, &["add", "-A"])?,
        ));
        transcript.push(format_git_step(
            &format!("git commit -m {:?}", message),
            run_git_command(cwd, &["commit", "-m", message])?,
        ));
    } else {
        transcript.push(
            "No uncommitted changes detected, so BelloSaize only pushed local commits.".to_string(),
        );
    }

    let current_branch = current_git_branch(cwd)?;
    let push_attempt = run_git_command(cwd, &["push"]);
    let push_result = match push_attempt {
        Ok(output) => ("git push".to_string(), output),
        Err(error) => {
            let rendered = format!("{error:#}");
            if rendered.contains("no upstream branch")
                || rendered.contains("has no upstream branch")
            {
                let fallback_args = ["push", "-u", "origin", current_branch.as_str()];
                (
                    format!("git push -u origin {}", current_branch),
                    run_git_command(cwd, &fallback_args)?,
                )
            } else {
                return Err(error);
            }
        }
    };

    transcript.push(format_git_step(&push_result.0, push_result.1));
    Ok(transcript.join("\n\n"))
}

fn execute_fetch(cwd: &Path) -> Result<(String, RepoStatus, RepoActionOutcome)> {
    let mut transcript = Vec::new();
    let has_remote = git_has_remote(cwd)?;
    if !has_remote {
        let status = inspect_project_without_remote_refresh(cwd);
        transcript.push("No remote configured, so there was nothing to fetch.".to_string());
        return Ok((transcript.join("\n\n"), status, RepoActionOutcome::Skipped));
    }

    transcript.push(format_git_step(
        "git fetch --quiet --all --prune",
        run_git_command(
            cwd,
            &[
                "-c",
                "credential.interactive=never",
                "fetch",
                "--quiet",
                "--all",
                "--prune",
            ],
        )?,
    ));

    let status_after_fetch = inspect_project_without_remote_refresh(cwd);
    transcript.push(format!(
        "Final status: {}",
        status_after_fetch.short_label()
    ));
    Ok((
        transcript.join("\n\n"),
        status_after_fetch,
        RepoActionOutcome::Applied,
    ))
}

fn execute_pull(cwd: &Path) -> Result<(String, RepoStatus, RepoActionOutcome)> {
    let mut transcript = Vec::new();
    let has_remote = git_has_remote(cwd)?;
    if !has_remote {
        let status = inspect_project_without_remote_refresh(cwd);
        transcript.push("No remote configured, so there was nothing to pull.".to_string());
        return Ok((transcript.join("\n\n"), status, RepoActionOutcome::Skipped));
    }

    transcript.push(format_git_step(
        "git fetch --quiet --all --prune",
        run_git_command(
            cwd,
            &[
                "-c",
                "credential.interactive=never",
                "fetch",
                "--quiet",
                "--all",
                "--prune",
            ],
        )?,
    ));

    let status_after_fetch = inspect_project_without_remote_refresh(cwd);
    if !status_after_fetch.available {
        transcript.push("Repository status is unavailable after fetch.".to_string());
        return Ok((
            transcript.join("\n\n"),
            status_after_fetch,
            RepoActionOutcome::Skipped,
        ));
    }

    if !status_after_fetch.has_upstream {
        transcript.push(
            "Fetched remotes, but skipped pull because the current branch has no upstream."
                .to_string(),
        );
        return Ok((
            transcript.join("\n\n"),
            status_after_fetch,
            RepoActionOutcome::Skipped,
        ));
    }

    if status_after_fetch.dirty {
        transcript.push(
            "Fetched remotes, but skipped pull because the working tree has uncommitted changes."
                .to_string(),
        );
        return Ok((
            transcript.join("\n\n"),
            status_after_fetch,
            RepoActionOutcome::Skipped,
        ));
    }

    if status_after_fetch.behind == 0 {
        transcript.push("Already up to date after fetch.".to_string());
        return Ok((
            transcript.join("\n\n"),
            status_after_fetch,
            RepoActionOutcome::Skipped,
        ));
    }

    if status_after_fetch.ahead > 0 {
        transcript.push(
            "Fetched remotes, but skipped pull because the branch has local commits and is not a clean fast-forward."
                .to_string(),
        );
        return Ok((
            transcript.join("\n\n"),
            status_after_fetch,
            RepoActionOutcome::Skipped,
        ));
    }

    transcript.push(format_git_step(
        "git pull --ff-only",
        run_git_command(
            cwd,
            &["-c", "credential.interactive=never", "pull", "--ff-only"],
        )?,
    ));

    let final_status = inspect_project_without_remote_refresh(cwd);
    transcript.push(format!("Final status: {}", final_status.short_label()));
    Ok((
        transcript.join("\n\n"),
        final_status,
        RepoActionOutcome::Applied,
    ))
}

fn execute_repo_action(action: RepoAction, project: &ProjectInfo) -> RepoActionReport {
    let result = match action {
        RepoAction::Fetch => execute_fetch(&project.path),
        RepoAction::Pull => execute_pull(&project.path),
    };

    match result {
        Ok((output, repo_status, outcome)) => RepoActionReport {
            name: project.name.clone(),
            path: project.path.clone(),
            repo_status,
            output,
            outcome,
        },
        Err(error) => RepoActionReport {
            name: project.name.clone(),
            path: project.path.clone(),
            repo_status: inspect_project(&project.path),
            output: format!("{error:#}"),
            outcome: RepoActionOutcome::Failed,
        },
    }
}

fn apply_repo_action_reports(state: &SharedState, reports: &[RepoActionReport]) {
    {
        let mut state = state.borrow_mut();
        for project in &mut state.projects {
            if let Some(report) = reports.iter().find(|report| report.path == project.path) {
                project.repo_status = report.repo_status.clone();
            }
        }
    }
    rebuild_project_list(state);
}

fn render_repo_action_report(action: RepoAction, reports: &[RepoActionReport]) -> String {
    reports
        .iter()
        .map(|report| {
            format!(
                "{} [{}]\nPath: {}\nStatus: {}\n\n{}",
                report.name,
                match report.outcome {
                    RepoActionOutcome::Applied => action.status_verb().to_uppercase(),
                    RepoActionOutcome::Skipped => "SKIPPED".to_string(),
                    RepoActionOutcome::Failed => "FAILED".to_string(),
                },
                report.path.display(),
                report.repo_status.short_label(),
                report.output,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n----------------------------------------\n\n")
}

fn summarize_repo_action(
    action: RepoAction,
    scope: RepoActionScope,
    reports: &[RepoActionReport],
) -> String {
    let applied = reports
        .iter()
        .filter(|report| matches!(report.outcome, RepoActionOutcome::Applied))
        .count();
    let skipped = reports
        .iter()
        .filter(|report| matches!(report.outcome, RepoActionOutcome::Skipped))
        .count();
    let failed = reports
        .iter()
        .filter(|report| matches!(report.outcome, RepoActionOutcome::Failed))
        .count();

    format!(
        "{} {} ({applied} {}, {skipped} skipped, {failed} failed)",
        action.label().to_lowercase(),
        describe_repo_scope_target_count(scope, reports.len()),
        action.status_verb()
    )
}

fn describe_repo_scope_target_count(scope: RepoActionScope, count: usize) -> String {
    match scope {
        RepoActionScope::Selected if count == 1 => "selected repo".to_string(),
        RepoActionScope::All => format!("{count} {}", if count == 1 { "repo" } else { "repos" }),
        RepoActionScope::Selected => {
            format!("{count} {}", if count == 1 { "repo" } else { "repos" })
        }
    }
}

fn git_has_remote(cwd: &Path) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["remote"])
        .output()
        .with_context(|| format!("failed to run `git remote` in {}", cwd.display()))?;

    if !output.status.success() {
        return Err(anyhow!(
            "git remote exited with status {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| !line.trim().is_empty()))
}

fn run_git_command(cwd: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args);

    let output = command.output().with_context(|| {
        format!(
            "failed to run `git {}` in {}",
            args.join(" "),
            cwd.display()
        )
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        let combined = [stdout, stderr]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(if combined.is_empty() {
            "Command finished without output.".to_string()
        } else {
            combined
        });
    }

    Err(anyhow!(
        "git {} exited with status {}{}\n{}",
        args.join(" "),
        output.status,
        if stdout.is_empty() { "" } else { ":" },
        [stdout, stderr]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

fn current_git_branch(cwd: &Path) -> Result<String> {
    let branch = run_git_command(cwd, &["branch", "--show-current"])?;
    let branch = branch.lines().next().unwrap_or("").trim().to_string();
    if branch.is_empty() {
        return Err(anyhow!("could not determine current git branch"));
    }
    Ok(branch)
}

fn format_git_step(command: &str, output: String) -> String {
    let body = if output.trim().is_empty() {
        "OK".to_string()
    } else {
        output
    };
    format!("{command}\n{body}")
}

#[allow(deprecated)]
fn show_output_dialog(window: &ApplicationWindow, title: &str, body: &str) {
    let dialog = gtk::Dialog::builder()
        .title(title)
        .transient_for(window)
        .modal(true)
        .build();
    dialog.add_button("Close", ResponseType::Close);
    dialog.connect_response(|dialog, _| dialog.close());

    let content = dialog.content_area();
    content.set_spacing(12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let scroller = ScrolledWindow::builder()
        .min_content_width(760)
        .min_content_height(320)
        .build();

    let text = TextView::new();
    text.set_editable(false);
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::WordChar);
    let buffer = TextBuffer::new(None);
    buffer.set_text(body);
    text.set_buffer(Some(&buffer));
    scroller.set_child(Some(&text));

    content.append(&scroller);
    dialog.present();
}

fn persist_sessions(state: &SharedState) -> Result<()> {
    let path = state.borrow().session_file_path.clone();
    save(
        &path,
        &SessionFile {
            sessions: Vec::new(),
        },
    )
}

fn parse_command(command: &str) -> Result<Vec<String>> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok(vec![crate::persist::default_shell()]);
    }

    shlex::split(trimmed).ok_or_else(|| anyhow!("invalid command line: {trimmed}"))
}

fn spawn_argv(spec: &SessionSpec) -> Result<Vec<String>> {
    let mut argv = parse_command(&spec.resolved_command())?;

    if let Some(binary) = profile_binary_name(spec.profile) {
        if argv.first().is_some_and(|arg| arg == binary) {
            if let Some(resolved_path) = resolve_binary_from_shell(binary) {
                argv[0] = resolved_path;
            }
        }
    }

    Ok(argv)
}

fn selected_project_path(state: &SharedState) -> Option<PathBuf> {
    let state = state.borrow();
    selected_project_ref(&state).map(|project| project.path.clone())
}

fn selected_project_ref(state: &AppState) -> Option<&ProjectInfo> {
    state
        .selected_project_index
        .and_then(|index| state.projects.get(index))
}

fn project_at_index(state: &SharedState, index: usize) -> Option<ProjectInfo> {
    state.borrow().projects.get(index).cloned()
}

fn selected_session_ref(state: &AppState) -> Option<&Rc<SessionView>> {
    let id = state.selected_session_id?;
    state.sessions.iter().find(|session| session.id == id)
}

fn selected_session(state: &SharedState) -> Option<Rc<SessionView>> {
    let state = state.borrow();
    selected_session_ref(&state).cloned()
}

fn commit_push_target_from_state(state: &AppState) -> Option<CommitPushTarget> {
    let selected_project = selected_project_ref(state)?;
    let selected_session = selected_session_ref(state);

    match selected_session {
        Some(session) => {
            let session_spec = session.spec.borrow();
            if path_is_within_repo(&session_spec.cwd, &selected_project.path) {
                return Some(CommitPushTarget {
                    cwd: session_spec.cwd.clone(),
                    title: session_spec.title(),
                });
            }

            Some(CommitPushTarget {
                cwd: selected_project.path.clone(),
                title: selected_project.name.clone(),
            })
        }
        None => Some(CommitPushTarget {
            cwd: selected_project.path.clone(),
            title: selected_project.name.clone(),
        }),
    }
}

fn commit_push_mode_from_state(state: &AppState) -> Option<CommitPushMode> {
    selected_project_ref(state)
        .and_then(|project| commit_push_mode_for_status(&project.repo_status))
}

fn commit_push_mode_for_status(status: &RepoStatus) -> Option<CommitPushMode> {
    if status.dirty {
        Some(CommitPushMode::CommitAndPush)
    } else if status.ahead > 0 {
        Some(CommitPushMode::PushOnly)
    } else {
        None
    }
}

fn path_is_within_repo(path: &Path, repo_root: &Path) -> bool {
    path == repo_root || path.starts_with(repo_root)
}

fn refresh_project_status_for_cwd(state: &SharedState, cwd: &Path) {
    let repo_path = {
        let state_ref = state.borrow();
        state_ref
            .projects
            .iter()
            .find(|project| path_is_within_repo(cwd, &project.path))
            .map(|project| project.path.clone())
    };

    let Some(repo_path) = repo_path else {
        return;
    };

    let refreshed_status = inspect_project(&repo_path);
    {
        let mut state_mut = state.borrow_mut();
        if let Some(project) = state_mut
            .projects
            .iter_mut()
            .find(|project| project.path == repo_path)
        {
            project.repo_status = refreshed_status;
        }
    }

    rebuild_project_list(state);
}

fn next_session_id(state: &SharedState) -> u64 {
    let mut state = state.borrow_mut();
    let next = state.next_session_id;
    state.next_session_id += 1;
    next
}

fn push_status(state: &SharedState, message: String) {
    push_status_with_details(state, message, None);
}

fn push_status_with_details(state: &SharedState, message: String, details: Option<String>) {
    let status_label = state.borrow().status_label.clone();
    status_label.set_text(&message);
    status_label.set_tooltip_text(details.as_deref());
}

fn kill_all_sessions(state: &SharedState) {
    for session in &state.borrow().sessions {
        kill_pid(session.pid.get());
    }
}

fn kill_pid(pid: Option<glib::Pid>) {
    if let Some(pid) = pid {
        // SAFETY: pid comes from VTE's child-spawn APIs.
        unsafe {
            libc::kill(pid.0, libc::SIGTERM);
        }
    }
}

fn binary_exists(binary: &str) -> bool {
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths)
            .map(|dir| dir.join(binary))
            .any(|candidate| candidate.is_file())
    })
}

fn profile_css_class(profile: Profile) -> &'static str {
    match profile {
        Profile::Shell => "profile-shell",
        Profile::Codex => "profile-codex",
        Profile::Claude => "profile-claude",
        Profile::Mistral => "profile-mistral",
        Profile::Custom => "profile-custom",
    }
}

fn profile_binary_name(profile: Profile) -> Option<&'static str> {
    match profile {
        Profile::Codex => Some("codex"),
        Profile::Claude => Some("claude"),
        Profile::Mistral => Some("mistral"),
        Profile::Shell | Profile::Custom => None,
    }
}

fn spawn_environment() -> Vec<String> {
    let mut envv = env::vars().collect::<BTreeMap<_, _>>();
    envv.extend(login_shell_environment());
    envv.insert("TERM".to_string(), "xterm-256color".to_string());
    envv.insert("COLORTERM".to_string(), "truecolor".to_string());
    envv.insert("SHELL".to_string(), crate::persist::default_shell());
    envv.into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

fn resolve_binary_from_shell(binary: &str) -> Option<String> {
    let shell = crate::persist::default_shell();
    let command = format!("command -v {binary}");
    let output = Command::new(&shell).args(["-lc", &command]).output().ok()?;

    if !output.status.success() {
        return None;
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with('/'))
        .map(ToOwned::to_owned)
}

fn login_shell_environment() -> BTreeMap<String, String> {
    let shell = crate::persist::default_shell();
    let output = Command::new(&shell).args(["-lc", "env -0"]).output();

    let Ok(output) = output else {
        return BTreeMap::new();
    };

    if !output.status.success() {
        return BTreeMap::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter_map(|entry| entry.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn apply_terminal_theme(terminal: &Terminal) {
    let font = FontDescription::from_string("JetBrains Mono 11");
    terminal.set_font(Some(&font));

    let foreground = gdk::RGBA::parse("#d4d4d4").unwrap_or_else(|_| gdk::RGBA::BLACK);
    let background = gdk::RGBA::parse("#1e1e1e").unwrap_or_else(|_| gdk::RGBA::WHITE);
    let palette_values = [
        "#1e1e1e", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#e5e5e5",
        "#666666", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#ffffff",
    ];
    let palette = palette_values
        .iter()
        .map(|value| gdk::RGBA::parse(*value).unwrap_or_else(|_| gdk::RGBA::BLACK))
        .collect::<Vec<_>>();
    let palette_refs = palette.iter().collect::<Vec<_>>();

    terminal.set_colors(Some(&foreground), Some(&background), &palette_refs);
}

fn apply_css() {
    let display = gdk::Display::default().expect("display should exist");
    let provider = CssProvider::new();
    provider.load_from_string(
        "
        .app-shell {
            background: #1e1e1e;
        }

        .sidebar {
            padding: 8px;
            background: #252526;
            border-right: 1px solid #2d2d2d;
        }

        .sidebar-header,
        .workspace-header,
        .workspace-stage,
        .session-card {
            background: #1e1e1e;
            border: 1px solid #2d2d2d;
            box-shadow: none;
        }

        .sidebar-header,
        .workspace-header {
            padding: 8px;
        }

        .sidebar-title,
        .workspace-title {
            color: #cccccc;
            font-size: 16px;
            font-weight: 700;
        }

        .count-label {
            color: #9da0a6;
            font-size: 11px;
            font-weight: 600;
        }

        .hint-label,
        .workspace-path,
        .workspace-repo-status,
        .project-path,
        .session-subtitle,
        .empty-body {
            color: #9da0a6;
            font-size: 12px;
        }

        .action-button {
            min-height: 32px;
            padding: 0 12px;
            border-radius: 3px;
            border: 1px solid #3c3c3c;
            background: #2d2d30;
            color: #cccccc;
            font-weight: 600;
            box-shadow: none;
        }

        .action-button:hover:not(:disabled) {
            background: #37373d;
        }

        .action-button:disabled {
            opacity: 0.45;
        }

        .primary-button:not(:disabled) {
            background: #0e639c;
            border-color: #1177bb;
            color: #ffffff;
        }

        .primary-button:hover:not(:disabled) {
            background: #1177bb;
        }

        .workspace {
            padding: 8px;
            background: #1e1e1e;
        }

        .workspace-header {
            padding: 8px;
        }

        .toolbar-group {
            padding: 0;
        }

        .workspace-stage {
            background: #1e1e1e;
        }

        .stage-scroller,
        .project-scroller,
        .project-list {
            background: transparent;
        }

        .project-row {
            background: transparent;
        }

        .project-row-body {
            padding: 8px 10px;
            border-radius: 3px;
            background: transparent;
            border: 1px solid transparent;
        }

        .project-row:hover .project-row-body {
            background: #2a2d2e;
        }

        .selected-project-row .project-row-body {
            background: #094771;
            border-color: #0e639c;
        }

        .project-name,
        .empty-title,
        .session-title {
            color: #cccccc;
            font-size: 14px;
            font-weight: 700;
        }

        .repo-status-label {
            padding: 2px 8px;
            border-radius: 999px;
            font-size: 10px;
            font-weight: 700;
        }

        .repo-state-ok {
            background: #183d2e;
            color: #b5f4d4;
        }

        .repo-state-warn {
            background: #4d3419;
            color: #ffd7a8;
        }

        .repo-state-alert {
            background: #4d1f24;
            color: #f2b8bd;
        }

        .repo-state-muted {
            background: #303030;
            color: #b7b7b7;
        }

        .empty-state {
            padding: 32px;
            border: 1px dashed #3c3c3c;
            background: #1e1e1e;
        }

        .session-card {
            background: #1e1e1e;
        }

        .session-header {
            padding: 8px 10px;
            background: #252526;
            border-bottom: 1px solid #2d2d2d;
        }

        .selected-card {
            border-color: #0e639c;
        }

        .profile-badge,
        .live-pill,
        .dead-pill {
            padding: 4px 8px;
            border-radius: 3px;
            font-size: 10px;
            font-weight: 700;
        }

        .profile-shell {
            background: #3c3c3c;
            color: #d4d4d4;
        }

        .profile-codex {
            background: #09395c;
            color: #9cdcfe;
        }

        .profile-claude {
            background: #4d3419;
            color: #ffd7a8;
        }

        .profile-mistral {
            background: #183d2e;
            color: #b5f4d4;
        }

        .profile-custom {
            background: #3a2b52;
            color: #d9c5ff;
        }

        .live-pill {
            background: #183d2e;
            color: #b5f4d4;
        }

        .dead-pill {
            background: #4d1f24;
            color: #f2b8bd;
        }

        .footer {
            padding: 4px 8px;
            background: #181818;
            border: 1px solid #2d2d2d;
        }

        .footer-status,
        .footer-count {
            color: #cccccc;
            font-size: 12px;
            font-weight: 600;
        }
        ",
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
