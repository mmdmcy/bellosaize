use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    env,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use gio::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CenterBox, CheckButton,
    CssProvider, DropDown, Entry, Grid, Label, ListBox, Orientation, Paned, ResponseType,
    ScrolledWindow, Stack, TextBuffer, TextView, gdk, prelude::*,
};
use pango::{EllipsizeMode, FontDescription};
use vte::{CursorBlinkMode, PtyFlags, Terminal, prelude::*};

use crate::{
    persist::{Profile, SessionFile, SessionSpec, load_or_bootstrap, save},
    project::{
        ProjectInfo, RepoStatus, default_roots, describe_pending_changes, discover_projects,
        inspect_project, inspect_project_without_remote_refresh,
    },
};

const APP_ID: &str = "com.mmdmcy.BelloSaize";

#[derive(Clone, Copy, Eq, PartialEq)]
enum RepoActionScope {
    Current,
    Marked,
    All,
}

impl RepoActionScope {
    fn from_index(index: u32) -> Self {
        match index {
            1 => Self::Marked,
            2 => Self::All,
            _ => Self::Current,
        }
    }
}

const GITHUB_UPDATE_LABEL: &str = "Get Up To Date";
const SPINNER_FRAMES: [&str; 4] = ["/", "-", "\\", "|"];

#[derive(Clone, Copy)]
enum RepoActionOutcome {
    Updated,
    UpToDate,
    Skipped,
    Failed,
}

#[derive(Clone, Copy)]
enum RepoUpdateMode {
    Safe,
    DiscardLocal,
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

    fn busy_kind(self) -> RepoBusyKind {
        match self {
            Self::CommitAndPush => RepoBusyKind::CommitPush,
            Self::PushOnly => RepoBusyKind::Push,
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RepoBusyKind {
    Sync,
    CommitPush,
    Push,
}

impl RepoBusyKind {
    fn status_label(self) -> &'static str {
        match self {
            Self::Sync => "Syncing",
            Self::CommitPush => "Commit+Push",
            Self::Push => "Push",
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
    repo_path: PathBuf,
    cwd: PathBuf,
    title: String,
}

#[derive(Clone)]
struct RepoBusyFeedback {
    kind: RepoBusyKind,
    target_paths: Vec<PathBuf>,
    frame: usize,
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
    refresh_button: Button,
    workspace_title_label: Label,
    workspace_path_label: Label,
    workspace_repo_status_label: Label,
    dirty_changes_panel: GtkBox,
    dirty_changes_title_label: Label,
    dirty_changes_buffer: TextBuffer,
    dirty_changes_refresh_button: Button,
    status_label: Label,
    count_label: Label,
    reset_button: Button,
    close_button: Button,
    zoom_button: Button,
    repo_scope_combo: DropDown,
    github_update_button: Button,
    commit_push_button: Button,
    project_status_labels: Vec<Label>,
    session_file_path: PathBuf,
    project_roots: Vec<PathBuf>,
    projects: Vec<ProjectInfo>,
    selected_project_index: Option<usize>,
    repo_action_target_paths: BTreeSet<PathBuf>,
    sessions: Vec<Rc<SessionView>>,
    selected_session_id: Option<u64>,
    zoomed_session_id: Option<u64>,
    sidebar_visible: bool,
    last_sidebar_width: i32,
    next_session_id: u64,
    repo_action_busy: bool,
    repo_busy_feedback: Option<RepoBusyFeedback>,
    refresh_busy: bool,
    busy_frame: usize,
    dirty_changes_for_path: Option<PathBuf>,
    dirty_changes_loading_path: Option<PathBuf>,
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
    github_update_button: Button,
    commit_push_button: Button,
}

struct WorkspaceWidgets {
    stack: Stack,
    grid: Grid,
    sidebar_toggle_button: Button,
    workspace_title_label: Label,
    workspace_path_label: Label,
    workspace_repo_status_label: Label,
    dirty_changes_panel: GtkBox,
    dirty_changes_title_label: Label,
    dirty_changes_buffer: TextBuffer,
    dirty_changes_refresh_button: Button,
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
        refresh_button: sidebar_widgets.refresh_button.clone(),
        workspace_title_label: workspace_widgets.workspace_title_label,
        workspace_path_label: workspace_widgets.workspace_path_label,
        workspace_repo_status_label: workspace_widgets.workspace_repo_status_label,
        dirty_changes_panel: workspace_widgets.dirty_changes_panel,
        dirty_changes_title_label: workspace_widgets.dirty_changes_title_label,
        dirty_changes_buffer: workspace_widgets.dirty_changes_buffer,
        dirty_changes_refresh_button: workspace_widgets.dirty_changes_refresh_button.clone(),
        status_label: workspace_widgets.status_label,
        count_label: workspace_widgets.count_label,
        reset_button: workspace_widgets.reset_button.clone(),
        close_button: workspace_widgets.close_button.clone(),
        zoom_button: workspace_widgets.zoom_button.clone(),
        repo_scope_combo: sidebar_widgets.repo_scope_combo.clone(),
        github_update_button: sidebar_widgets.github_update_button.clone(),
        commit_push_button: sidebar_widgets.commit_push_button.clone(),
        project_status_labels: Vec::new(),
        session_file_path,
        project_roots,
        projects: Vec::new(),
        selected_project_index: None,
        repo_action_target_paths: BTreeSet::new(),
        sessions: Vec::new(),
        selected_session_id: None,
        zoomed_session_id: None,
        sidebar_visible: true,
        last_sidebar_width: 260,
        next_session_id: 1,
        repo_action_busy: false,
        repo_busy_feedback: None,
        refresh_busy: false,
        busy_frame: 0,
        dirty_changes_for_path: None,
        dirty_changes_loading_path: None,
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
        let github_update_button = state.borrow().github_update_button.clone();
        github_update_button.connect_clicked(move |_| run_repo_update_for_scope(&state));
    }
    {
        let state = state.clone();
        let commit_push_button = state.borrow().commit_push_button.clone();
        commit_push_button.connect_clicked(move |_| prompt_commit_and_push(&state));
    }
    {
        let state = state.clone();
        let dirty_changes_refresh_button = state.borrow().dirty_changes_refresh_button.clone();
        dirty_changes_refresh_button.connect_clicked(move |_| refresh_dirty_changes_panel(&state));
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

    let refresh_button = action_button("Refresh All");
    refresh_button.set_tooltip_text(Some(
        "Rescan configured roots and refresh status for every discovered repo. This does not pull or push.",
    ));
    refresh_button.set_hexpand(true);

    let repo_scope_combo = DropDown::from_strings(&["Current", "Marked", "All"]);
    repo_scope_combo.set_selected(0);
    repo_scope_combo.set_hexpand(true);
    repo_scope_combo.set_tooltip_text(Some(
        "Choose whether Get Up To Date targets the current repo, checked repos, or every discovered repo.",
    ));

    let scope_row = GtkBox::new(Orientation::Horizontal, 6);
    scope_row.add_css_class("sidebar-toolbar-row");
    scope_row.append(&refresh_button);
    scope_row.append(&repo_scope_combo);

    let github_update_button = action_button(GITHUB_UPDATE_LABEL);
    github_update_button.set_hexpand(true);
    github_update_button.set_tooltip_text(Some(
        "Fetch remote changes for the chosen target, fast-forward safe repos, and ask before discarding local work.",
    ));
    let commit_push_button = action_button("Commit+Push");
    commit_push_button.set_hexpand(true);
    commit_push_button.add_css_class("primary-button");
    commit_push_button.set_tooltip_text(Some(
        "Commit pending changes, or push existing local commits, in the selected repo.",
    ));

    let git_row = GtkBox::new(Orientation::Horizontal, 6);
    git_row.add_css_class("sidebar-toolbar-row");
    git_row.append(&github_update_button);
    git_row.append(&commit_push_button);

    title_row.append(&title);
    title_row.append(&project_count_label);

    let hint = Label::new(Some(
        "Click a repo to focus it. Tick repos to target a marked batch. Double-click opens a shell.",
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
            github_update_button,
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

    let (
        dirty_changes_panel,
        dirty_changes_title_label,
        dirty_changes_buffer,
        dirty_changes_refresh_button,
    ) = build_dirty_changes_panel();

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
    workspace.append(&dirty_changes_panel);
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
            dirty_changes_panel,
            dirty_changes_title_label,
            dirty_changes_buffer,
            dirty_changes_refresh_button,
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

fn build_dirty_changes_panel() -> (GtkBox, Label, TextBuffer, Button) {
    let panel = GtkBox::new(Orientation::Vertical, 6);
    panel.add_css_class("dirty-changes-panel");
    panel.set_visible(false);

    let header = GtkBox::new(Orientation::Horizontal, 8);
    let title = Label::new(Some("Uncommitted changes"));
    title.add_css_class("dirty-changes-title");
    title.set_xalign(0.0);
    title.set_hexpand(true);

    let refresh_button = action_button("Refresh");
    refresh_button.set_tooltip_text(Some("Reload the selected repo's uncommitted changes."));

    header.append(&title);
    header.append(&refresh_button);

    let buffer = TextBuffer::new(None);
    let text = TextView::new();
    text.add_css_class("dirty-changes-text");
    text.set_buffer(Some(&buffer));
    text.set_editable(false);
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::None);

    let scroller = ScrolledWindow::builder()
        .min_content_height(190)
        .hexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&text)
        .build();
    scroller.add_css_class("dirty-changes-scroller");

    panel.append(&header);
    panel.append(&scroller);
    (panel, title, buffer, refresh_button)
}

fn action_button(label: &str) -> Button {
    let button = Button::with_label(label);
    button.add_css_class("action-button");
    button
}

fn refresh_projects(state: &SharedState) {
    let (previous_path, roots, busy) = {
        let state = state.borrow();
        (
            selected_project_ref(&state).map(|project| project.path.to_string_lossy().to_string()),
            state.project_roots.clone(),
            state.refresh_busy || state.repo_action_busy,
        )
    };
    if busy {
        return;
    }

    set_refresh_busy(state, true);
    let spinner_id = start_status_spinner(
        state,
        "Refreshing repositories".to_string(),
        Some(format_project_roots(&roots)),
    );

    let job = gio::spawn_blocking(move || load_projects(&roots));
    let state = state.clone();
    glib::MainContext::default().spawn_local(async move {
        match job.await {
            Ok(projects) => {
                let refresh_summary = describe_project_overview(&projects);
                {
                    let mut state_mut = state.borrow_mut();
                    state_mut.projects = projects;
                    let available_paths = state_mut
                        .projects
                        .iter()
                        .map(|project| project.path.clone())
                        .collect::<BTreeSet<_>>();
                    state_mut
                        .repo_action_target_paths
                        .retain(|path| available_paths.contains(path));
                    state_mut.dirty_changes_for_path = None;
                    state_mut.selected_project_index = previous_path.and_then(|path| {
                        state_mut
                            .projects
                            .iter()
                            .position(|project| project.path.to_string_lossy() == path)
                    });
                }

                spinner_id.remove();
                set_refresh_busy(&state, false);
                rebuild_project_list(&state);
                push_status(&state, refresh_summary);
            }
            Err(_) => {
                spinner_id.remove();
                set_refresh_busy(&state, false);
                push_status_with_details(
                    &state,
                    "Refresh failed".to_string(),
                    Some("Background refresh task panicked.".to_string()),
                );
            }
        }
    });
}

fn load_projects(roots: &[PathBuf]) -> Vec<ProjectInfo> {
    let mut projects = discover_projects(roots);
    if projects.is_empty()
        && let Ok(cwd) = env::current_dir()
    {
        projects.push(ProjectInfo {
            name: cwd
                .file_name()
                .and_then(|part| part.to_str())
                .unwrap_or("current")
                .to_string(),
            repo_status: RepoStatus::default(),
            path: cwd,
        });
    }

    for project in &mut projects {
        project.repo_status = inspect_project(&project.path);
    }

    projects
}

fn format_project_roots(roots: &[PathBuf]) -> String {
    let body = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    format!("Scanning configured roots:\n{body}")
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

    let mut status_labels = Vec::with_capacity(projects_for_rows.len());
    for (index, project) in projects_for_rows.iter().enumerate() {
        let (row, status_label) = build_project_row(state, index, project);
        status_labels.push(status_label);
        project_list.append(&row);
    }

    state.borrow_mut().project_status_labels = status_labels;

    if let Some(index) = selected_index {
        select_project_row(state, index);
    } else {
        update_project_ui(state);
    }
}

fn build_project_row(
    state: &SharedState,
    index: usize,
    project: &ProjectInfo,
) -> (gtk::ListBoxRow, Label) {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("project-row");
    row.set_tooltip_text(Some(&format!(
        "{}\nGit: {}",
        project.path.display(),
        project.repo_status.short_label()
    )));

    let body = GtkBox::new(Orientation::Horizontal, 8);
    body.add_css_class("project-row-body");
    body.set_hexpand(true);

    let marked_for_action = state
        .borrow()
        .repo_action_target_paths
        .contains(&project.path);
    let target_check = CheckButton::new();
    target_check.add_css_class("repo-target-check");
    target_check.set_tooltip_text(Some("Mark this repo for batch Get Up To Date actions."));
    target_check.set_active(marked_for_action);
    target_check.set_valign(Align::Center);
    {
        let state = state.clone();
        let project_name = project.name.clone();
        let project_path = project.path.clone();
        target_check.connect_toggled(move |button| {
            set_repo_action_target(
                &state,
                project_name.clone(),
                project_path.clone(),
                button.is_active(),
            );
        });
    }

    let text_body = GtkBox::new(Orientation::Vertical, 2);
    text_body.set_hexpand(true);

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

    text_body.append(&title_row);
    text_body.append(&path);
    body.append(&target_check);
    body.append(&text_body);

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
    text_body.add_controller(click);
    row.set_child(Some(&body));
    (row, repo_status)
}

fn update_project_ui(state: &SharedState) {
    let (
        project_list,
        project_count_label,
        refresh_button,
        repo_scope_combo,
        workspace_title_label,
        workspace_path_label,
        workspace_repo_status_label,
        github_update_button,
        commit_push_button,
        project_status_labels,
        projects_for_status,
        repo_busy_feedback,
        selected_index,
        repo_action_target_paths,
        project_total,
        action_target_count,
        repo_action_scope,
        repo_action_busy,
        refresh_busy,
        busy_frame,
        commit_push_enabled,
        selected_title,
        selected_path,
        selected_status,
        selected_status_busy,
    ) = {
        let state = state.borrow();
        let selected = selected_project_ref(&state);
        let selected_status_busy = selected.is_some_and(|project| {
            repo_feedback_targets_project(state.repo_busy_feedback.as_ref(), &project.path)
        });
        (
            state.project_list.clone(),
            state.project_count_label.clone(),
            state.refresh_button.clone(),
            state.repo_scope_combo.clone(),
            state.workspace_title_label.clone(),
            state.workspace_path_label.clone(),
            state.workspace_repo_status_label.clone(),
            state.github_update_button.clone(),
            state.commit_push_button.clone(),
            state.project_status_labels.clone(),
            state.projects.clone(),
            state.repo_busy_feedback.clone(),
            state.selected_project_index,
            state.repo_action_target_paths.clone(),
            state.projects.len(),
            repo_action_targets_from_state(&state).len(),
            current_repo_action_scope(&state),
            state.repo_action_busy,
            state.refresh_busy,
            state.busy_frame,
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
                .map(|project| {
                    workspace_repo_status_text(project, state.repo_busy_feedback.as_ref())
                })
                .unwrap_or_else(|| "Select a repository to inspect its git state.".to_string()),
            selected_status_busy,
        )
    };
    let app_busy = repo_action_busy || refresh_busy;
    let repo_busy_kind = repo_busy_feedback.as_ref().map(|feedback| feedback.kind);
    let repo_busy_frame = repo_busy_feedback
        .as_ref()
        .map(|feedback| feedback.frame)
        .unwrap_or(busy_frame);

    project_count_label.set_text(&describe_project_count(project_total));
    refresh_button.set_label(&if refresh_busy {
        spinner_label(busy_frame, "Refreshing")
    } else {
        "Refresh All".to_string()
    });
    if refresh_busy {
        refresh_button.add_css_class("busy-button");
    } else {
        refresh_button.remove_css_class("busy-button");
    }
    refresh_button.set_sensitive(!app_busy);
    project_list.set_sensitive(!app_busy);
    repo_scope_combo.set_sensitive(!app_busy);
    workspace_title_label.set_text(&selected_title);
    workspace_path_label.set_text(&selected_path);
    workspace_repo_status_label.set_text(&selected_status);
    if selected_status_busy {
        workspace_repo_status_label.add_css_class("workspace-repo-status-busy");
    } else {
        workspace_repo_status_label.remove_css_class("workspace-repo-status-busy");
    }
    let github_update_label = if repo_busy_kind == Some(RepoBusyKind::Sync) {
        spinner_label(repo_busy_frame, "Syncing")
    } else {
        match repo_action_scope {
            RepoActionScope::Current => "Update Current".to_string(),
            RepoActionScope::Marked => format!("Update Marked ({action_target_count})"),
            RepoActionScope::All => "Update All".to_string(),
        }
    };
    github_update_button.set_label(&github_update_label);
    let commit_push_label = match repo_busy_kind {
        Some(RepoBusyKind::CommitPush) => spinner_label(repo_busy_frame, "Commit+Push"),
        Some(RepoBusyKind::Push) => spinner_label(repo_busy_frame, "Pushing"),
        _ => "Commit+Push".to_string(),
    };
    commit_push_button.set_label(&commit_push_label);
    if repo_busy_kind == Some(RepoBusyKind::Sync) {
        github_update_button.add_css_class("busy-button");
    } else {
        github_update_button.remove_css_class("busy-button");
    }
    if matches!(
        repo_busy_kind,
        Some(RepoBusyKind::CommitPush | RepoBusyKind::Push)
    ) {
        commit_push_button.add_css_class("busy-button");
    } else {
        commit_push_button.remove_css_class("busy-button");
    }
    github_update_button.set_sensitive(!app_busy && action_target_count > 0);
    commit_push_button.set_sensitive(!app_busy && commit_push_enabled);
    update_project_status_labels(
        &project_status_labels,
        &projects_for_status,
        repo_busy_feedback.as_ref(),
    );
    update_project_row_classes(
        &project_list,
        &projects_for_status,
        selected_index,
        &repo_action_target_paths,
    );
    sync_dirty_changes_panel(state);
}

fn sync_dirty_changes_panel(state: &SharedState) {
    let (
        panel,
        title_label,
        buffer,
        refresh_button,
        selected_project,
        changes_for_path,
        loading_path,
        busy_frame,
        app_busy,
    ) = {
        let state_ref = state.borrow();
        (
            state_ref.dirty_changes_panel.clone(),
            state_ref.dirty_changes_title_label.clone(),
            state_ref.dirty_changes_buffer.clone(),
            state_ref.dirty_changes_refresh_button.clone(),
            selected_project_ref(&state_ref).cloned(),
            state_ref.dirty_changes_for_path.clone(),
            state_ref.dirty_changes_loading_path.clone(),
            state_ref.busy_frame,
            state_ref.repo_action_busy || state_ref.refresh_busy,
        )
    };

    let Some(project) = selected_project else {
        hide_dirty_changes_panel(state, &panel, &buffer);
        return;
    };

    if !project.repo_status.dirty {
        hide_dirty_changes_panel(state, &panel, &buffer);
        return;
    }

    let loading_selected = loading_path
        .as_ref()
        .is_some_and(|path| path == &project.path);

    panel.set_visible(true);
    title_label.set_text(&format!("Uncommitted changes: {}", project.name));
    refresh_button.set_label(&if loading_selected {
        spinner_label(busy_frame, "Refreshing")
    } else {
        "Refresh".to_string()
    });
    if loading_selected {
        refresh_button.add_css_class("busy-button");
    } else {
        refresh_button.remove_css_class("busy-button");
    }
    refresh_button.set_sensitive(!app_busy && !loading_selected);

    if loading_selected
        || changes_for_path
            .as_ref()
            .is_some_and(|path| path == &project.path)
        || app_busy
    {
        return;
    }

    start_dirty_changes_load(state, project.path, project.name);
}

fn hide_dirty_changes_panel(state: &SharedState, panel: &GtkBox, buffer: &TextBuffer) {
    panel.set_visible(false);
    buffer.set_text("");

    let mut state_mut = state.borrow_mut();
    state_mut.dirty_changes_for_path = None;
}

fn refresh_dirty_changes_panel(state: &SharedState) {
    let selected_project = {
        let state_ref = state.borrow();
        selected_project_ref(&state_ref)
            .filter(|project| project.repo_status.dirty)
            .cloned()
    };

    if let Some(project) = selected_project {
        start_dirty_changes_load(state, project.path, project.name);
    }
}

fn start_dirty_changes_load(state: &SharedState, path: PathBuf, name: String) {
    let (buffer, refresh_button) = {
        let mut state_mut = state.borrow_mut();
        state_mut.dirty_changes_for_path = Some(path.clone());
        state_mut.dirty_changes_loading_path = Some(path.clone());
        state_mut.busy_frame = 0;
        (
            state_mut.dirty_changes_buffer.clone(),
            state_mut.dirty_changes_refresh_button.clone(),
        )
    };

    buffer.set_text("Loading uncommitted changes...");
    refresh_button.set_label(&spinner_label(0, "Refreshing"));
    refresh_button.add_css_class("busy-button");
    refresh_button.set_sensitive(false);
    let spinner_id = start_busy_button_spinner(state);

    let job_path = path.clone();
    let job = gio::spawn_blocking(move || describe_pending_changes(&job_path));
    let state = state.clone();
    glib::MainContext::default().spawn_local(async move {
        let result = job.await;
        spinner_id.remove();
        let (buffer, still_selected) = {
            let mut state_mut = state.borrow_mut();
            if state_mut
                .dirty_changes_loading_path
                .as_ref()
                .is_some_and(|loading_path| loading_path == &path)
            {
                state_mut.dirty_changes_loading_path = None;
            }

            let still_selected = selected_project_ref(&state_mut)
                .is_some_and(|project| project.path == path && project.repo_status.dirty);
            (state_mut.dirty_changes_buffer.clone(), still_selected)
        };

        if still_selected {
            let report = match result {
                Ok(Ok(report)) => report,
                Ok(Err(error)) => {
                    format!("Could not load changes for {}.\n\n{error}", path.display())
                }
                Err(_) => format!(
                    "Could not load changes for {}.\n\nBackground git task panicked.",
                    path.display()
                ),
            };
            buffer.set_text(&report);
            push_status(&state, format!("loaded changes for {name}"));
        }

        update_project_ui(&state);
    });
}

fn workspace_repo_status_text(
    project: &ProjectInfo,
    feedback: Option<&RepoBusyFeedback>,
) -> String {
    format!(
        "Git: {}",
        repo_status_label_text(project, feedback)
            .unwrap_or_else(|| project.repo_status.short_label())
    )
}

fn update_project_status_labels(
    labels: &[Label],
    projects: &[ProjectInfo],
    feedback: Option<&RepoBusyFeedback>,
) {
    for (label, project) in labels.iter().zip(projects) {
        let busy = repo_feedback_targets_project(feedback, &project.path);
        label.set_text(
            &repo_status_label_text(project, feedback)
                .unwrap_or_else(|| project.repo_status.short_label()),
        );
        apply_repo_status_label_class(label, &project.repo_status, busy);
    }
}

fn repo_status_label_text(
    project: &ProjectInfo,
    feedback: Option<&RepoBusyFeedback>,
) -> Option<String> {
    let feedback = feedback?;
    if !repo_feedback_targets_project(Some(feedback), &project.path) {
        return None;
    }

    Some(format!(
        "{} {}",
        SPINNER_FRAMES[feedback.frame],
        feedback.kind.status_label()
    ))
}

fn spinner_label(frame: usize, label: &str) -> String {
    format!("{} {}", SPINNER_FRAMES[frame % SPINNER_FRAMES.len()], label)
}

fn repo_feedback_targets_project(feedback: Option<&RepoBusyFeedback>, project_path: &Path) -> bool {
    feedback.is_some_and(|feedback| {
        feedback
            .target_paths
            .iter()
            .any(|target_path| target_path == project_path)
    })
}

fn apply_repo_status_label_class(label: &Label, status: &RepoStatus, busy: bool) {
    for class_name in [
        "repo-state-ok",
        "repo-state-warn",
        "repo-state-alert",
        "repo-state-muted",
        "repo-state-busy",
    ] {
        label.remove_css_class(class_name);
    }

    label.add_css_class(if busy {
        "repo-state-busy"
    } else {
        status.css_class()
    });
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

fn update_project_row_classes(
    project_list: &ListBox,
    projects: &[ProjectInfo],
    selected_index: Option<usize>,
    repo_action_target_paths: &BTreeSet<PathBuf>,
) {
    let mut index = 0;
    while let Some(row) = project_list.row_at_index(index) {
        let project_index = usize::try_from(index).ok();
        if selected_index == project_index {
            row.add_css_class("selected-project-row");
        } else {
            row.remove_css_class("selected-project-row");
        }
        if project_index
            .and_then(|index| projects.get(index))
            .is_some_and(|project| repo_action_target_paths.contains(&project.path))
        {
            row.add_css_class("targeted-project-row");
        } else {
            row.remove_css_class("targeted-project-row");
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

fn set_repo_action_target(
    state: &SharedState,
    project_name: String,
    project_path: PathBuf,
    targeted: bool,
) {
    let (target_count, repo_scope_combo) = {
        let mut state_mut = state.borrow_mut();
        if targeted {
            state_mut
                .repo_action_target_paths
                .insert(project_path.clone());
        } else {
            state_mut.repo_action_target_paths.remove(&project_path);
        }
        (
            state_mut.repo_action_target_paths.len(),
            state_mut.repo_scope_combo.clone(),
        )
    };

    if targeted {
        repo_scope_combo.set_selected(1);
    }

    let action = if targeted { "marked" } else { "unmarked" };
    push_status(
        state,
        format!("{action} repo for batch actions: {project_name} ({target_count} marked)"),
    );
    update_project_ui(state);
}

fn current_repo_action_scope(state: &AppState) -> RepoActionScope {
    RepoActionScope::from_index(state.repo_scope_combo.selected())
}

fn repo_action_targets_from_state(state: &AppState) -> Vec<ProjectInfo> {
    match current_repo_action_scope(state) {
        RepoActionScope::Current => selected_project_ref(state).cloned().into_iter().collect(),
        RepoActionScope::Marked => state
            .projects
            .iter()
            .filter(|project| state.repo_action_target_paths.contains(&project.path))
            .cloned()
            .collect(),
        RepoActionScope::All => state.projects.clone(),
    }
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
    let app_busy = {
        let state = state.borrow();
        state.repo_action_busy || state.refresh_busy
    };
    if app_busy {
        return;
    }

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

    let validation_label = Label::new(Some("Enter a commit message before continuing."));
    validation_label.add_css_class("hint-label");
    validation_label.set_xalign(0.0);
    validation_label.set_wrap(true);
    validation_label.set_visible(false);
    content.append(&validation_label);

    dialog.set_default_response(ResponseType::Accept);
    dialog.set_response_sensitive(ResponseType::Accept, false);

    {
        let dialog = dialog.clone();
        let validation_label = validation_label.clone();
        entry.connect_changed(move |entry| {
            let has_message = !entry.text().trim().is_empty();
            dialog.set_response_sensitive(ResponseType::Accept, has_message);
            if has_message {
                validation_label.set_visible(false);
            }
        });
    }

    {
        let state = state.clone();
        let entry = entry.clone();
        let target = target.clone();
        let validation_label = validation_label.clone();
        dialog.connect_response(move |dialog, response| {
            if response == ResponseType::Accept {
                let message = entry.text().trim().to_string();
                if message.is_empty() {
                    validation_label.set_visible(true);
                    entry.grab_focus();
                    return;
                }

                run_commit_and_push(
                    &state,
                    target.clone(),
                    CommitPushMode::CommitAndPush,
                    Some(message),
                );
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
            if let Some(uri) = term.current_directory_uri()
                && let Some(path) = gio::File::for_uri(&uri).path()
            {
                session.spec.borrow_mut().cwd = path.clone();
                session
                    .subtitle_label
                    .set_text(&session.spec.borrow().subtitle());
                let _ = persist_sessions(&state);
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
    let app_busy = {
        let state = state.borrow();
        state.repo_action_busy || state.refresh_busy
    };
    if app_busy {
        return;
    }

    set_repo_action_busy(state, true);

    let pending_message = match mode {
        CommitPushMode::CommitAndPush => format!("Commit+Push running for {}", target.title),
        CommitPushMode::PushOnly => format!("Push running for {}", target.title),
    };
    let spinner_id = start_status_spinner(
        state,
        pending_message,
        Some(format!("Path: {}", target.cwd.display())),
    );
    let repo_spinner_id =
        start_repo_status_spinner(state, vec![target.repo_path.clone()], mode.busy_kind());

    let cwd = target.cwd.clone();
    let job = gio::spawn_blocking(move || execute_commit_and_push(&cwd, message.as_deref()));

    let state = state.clone();
    glib::MainContext::default().spawn_local(async move {
        let result = job.await;
        spinner_id.remove();
        stop_repo_status_spinner(&state, repo_spinner_id);
        match result {
            Ok((result, repo_status)) => {
                apply_repo_status_for_cwd(&state, &target.cwd, repo_status);
                set_repo_action_busy(&state, false);

                match result {
                    Ok(output) => push_status_with_details(
                        &state,
                        format!("{} finished for {}", mode.report_title(), target.title),
                        Some(format!("Path: {}\n\n{}", target.cwd.display(), output)),
                    ),
                    Err(error) => push_status_with_details(
                        &state,
                        format!(
                            "{} failed for {}: {}",
                            mode.report_title(),
                            target.title,
                            first_error_line(&error)
                        ),
                        Some(format!("Path: {}\n\n{error:#}", target.cwd.display())),
                    ),
                }
            }
            Err(_) => {
                set_repo_action_busy(&state, false);
                push_status_with_details(
                    &state,
                    format!("{} failed for {}", mode.report_title(), target.title),
                    Some(format!(
                        "Path: {}\n\nBackground git task panicked.",
                        target.cwd.display()
                    )),
                );
            }
        }
    });
}

fn run_repo_update_for_scope(state: &SharedState) {
    let (scope, targets, busy) = {
        let state = state.borrow();
        (
            current_repo_action_scope(&state),
            repo_action_targets_from_state(&state),
            state.repo_action_busy || state.refresh_busy,
        )
    };
    if busy || targets.is_empty() {
        return;
    }

    let discard_targets = targets
        .iter()
        .filter(|project| repo_update_needs_discard_confirmation(&project.repo_status))
        .cloned()
        .collect::<Vec<_>>();
    if !discard_targets.is_empty() {
        prompt_discarding_repo_update(state, scope, targets, discard_targets);
        return;
    }

    run_repo_update(state, scope, targets, Vec::new());
}

#[allow(deprecated)]
fn prompt_discarding_repo_update(
    state: &SharedState,
    scope: RepoActionScope,
    targets: Vec<ProjectInfo>,
    discard_targets: Vec<ProjectInfo>,
) {
    let window = state.borrow().window.clone();
    let dialog = gtk::Dialog::builder()
        .title("Discard Local Work?")
        .transient_for(&window)
        .modal(true)
        .build();
    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("Discard And Update", ResponseType::Accept);
    dialog.set_default_response(ResponseType::Cancel);

    let content = dialog.content_area();
    content.set_spacing(10);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let warning = Label::new(Some(
        "Some targeted repos have uncommitted changes or local commits. Continuing will reset those branches to their upstream, discard uncommitted changes, delete untracked files, and drop local commits.",
    ));
    warning.set_wrap(true);
    warning.set_xalign(0.0);
    content.append(&warning);

    let target_list = Label::new(Some(&format_discard_update_targets(&discard_targets)));
    target_list.add_css_class("hint-label");
    target_list.set_selectable(true);
    target_list.set_wrap(true);
    target_list.set_xalign(0.0);
    content.append(&target_list);

    {
        let state = state.clone();
        dialog.connect_response(move |dialog, response| {
            if response == ResponseType::Accept {
                let discard_paths = discard_targets
                    .iter()
                    .map(|project| project.path.clone())
                    .collect::<Vec<_>>();
                run_repo_update(&state, scope, targets.clone(), discard_paths);
            } else {
                push_status(&state, format!("{GITHUB_UPDATE_LABEL} cancelled"));
            }
            dialog.close();
        });
    }

    dialog.present();
}

fn repo_update_needs_discard_confirmation(status: &RepoStatus) -> bool {
    status.available
        && status.has_remote
        && status.has_upstream
        && (status.dirty || status.ahead > 0)
}

fn format_discard_update_targets(targets: &[ProjectInfo]) -> String {
    targets
        .iter()
        .map(|project| {
            format!(
                "{} ({})\n{}",
                project.name,
                project.repo_status.short_label(),
                project.path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn run_repo_update(
    state: &SharedState,
    scope: RepoActionScope,
    targets: Vec<ProjectInfo>,
    discard_paths: Vec<PathBuf>,
) {
    set_repo_action_busy(state, true);
    let spinner_id = start_status_spinner(
        state,
        format!(
            "{} for {}",
            GITHUB_UPDATE_LABEL,
            describe_repo_scope_target_count(scope, targets.len())
        ),
        Some(format_repo_action_targets(&targets)),
    );
    let repo_spinner_id = start_repo_status_spinner(
        state,
        targets
            .iter()
            .map(|project| project.path.clone())
            .collect::<Vec<_>>(),
        RepoBusyKind::Sync,
    );

    let job = gio::spawn_blocking(move || {
        targets
            .into_iter()
            .map(|project| {
                let mode = if discard_paths
                    .iter()
                    .any(|discard_path| discard_path == &project.path)
                {
                    RepoUpdateMode::DiscardLocal
                } else {
                    RepoUpdateMode::Safe
                };
                execute_repo_update(&project, mode)
            })
            .collect::<Vec<_>>()
    });

    let state = state.clone();
    glib::MainContext::default().spawn_local(async move {
        let result = job.await;
        spinner_id.remove();
        stop_repo_status_spinner(&state, repo_spinner_id);
        match result {
            Ok(reports) => {
                apply_repo_action_reports(&state, &reports);
                set_repo_action_busy(&state, false);

                let summary = summarize_repo_update(scope, &reports);
                let details = render_repo_update_report(&reports);
                push_status_with_details(&state, summary, Some(details));
            }
            Err(_) => {
                set_repo_action_busy(&state, false);
                push_status_with_details(
                    &state,
                    format!("{GITHUB_UPDATE_LABEL} failed"),
                    Some("Background git task panicked.".to_string()),
                );
            }
        }
    });
}

fn format_repo_action_targets(targets: &[ProjectInfo]) -> String {
    targets
        .iter()
        .map(|project| format!("{}: {}", project.name, project.path.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn execute_commit_and_push(cwd: &Path, message: Option<&str>) -> (Result<String>, RepoStatus) {
    let result = execute_commit_and_push_inner(cwd, message);
    let repo_status = inspect_project(cwd);
    (result, repo_status)
}

fn execute_commit_and_push_inner(cwd: &Path, message: Option<&str>) -> Result<String> {
    let mut transcript = Vec::new();

    if let Some(message) = message {
        if let Some(identity_step) = ensure_git_commit_identity(cwd)? {
            transcript.push(identity_step);
        }
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

struct GithubCliIdentity {
    login: String,
    id: String,
}

impl GithubCliIdentity {
    fn noreply_email(&self) -> String {
        format!("{}+{}@users.noreply.github.com", self.id, self.login)
    }
}

fn ensure_git_commit_identity(cwd: &Path) -> Result<Option<String>> {
    if run_git_command(cwd, &["var", "GIT_AUTHOR_IDENT"]).is_ok() {
        return Ok(None);
    }

    let identity = github_cli_identity_for_repo(cwd).context(
        "git commit identity is not configured, and BelloSaize could not configure one from GitHub CLI; set user.name and user.email or run gh auth login",
    )?;
    let email = identity.noreply_email();

    run_git_command(cwd, &["config", "--local", "user.name", &identity.login])
        .context("failed to set repo-local git user.name")?;
    run_git_command(cwd, &["config", "--local", "user.email", &email])
        .context("failed to set repo-local git user.email")?;
    run_git_command(cwd, &["var", "GIT_AUTHOR_IDENT"])
        .context("repo-local git identity was configured but git still cannot create commits")?;

    Ok(Some(format!(
        "Configured repo-local Git identity from GitHub CLI\nuser.name={}\nuser.email={}",
        identity.login, email
    )))
}

fn github_cli_identity_for_repo(cwd: &Path) -> Result<GithubCliIdentity> {
    if !repo_has_github_remote(cwd)? {
        return Err(anyhow!(
            "no github.com remote is configured for this repository"
        ));
    }

    let output = run_gh_command(
        cwd,
        &["api", "user", "--jq", ".login + \"\\n\" + (.id | tostring)"],
    )?;
    let mut lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let login = lines
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("GitHub CLI did not return a login"))?
        .to_string();
    let id = lines
        .next()
        .filter(|value| value.chars().all(|ch| ch.is_ascii_digit()))
        .ok_or_else(|| anyhow!("GitHub CLI did not return a numeric user id"))?
        .to_string();

    Ok(GithubCliIdentity { login, id })
}

fn repo_has_github_remote(cwd: &Path) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["remote", "-v"])
        .output()
        .with_context(|| format!("failed to run `git remote -v` in {}", cwd.display()))?;

    if !output.status.success() {
        return Err(anyhow!(
            "git remote -v exited with status {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(remote_line_is_github))
}

fn remote_line_is_github(line: &str) -> bool {
    line.contains("github.com/") || line.contains("github.com:")
}

fn run_gh_command(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("gh")
        .current_dir(cwd)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .args(args)
        .output()
        .with_context(|| {
            format!(
                "failed to run `gh {}` in {}; install GitHub CLI and run gh auth login",
                args.join(" "),
                cwd.display()
            )
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        return Ok(stdout);
    }

    Err(anyhow!(
        "gh {} exited with status {}{}\n{}",
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

fn execute_github_update(
    cwd: &Path,
    mode: RepoUpdateMode,
) -> Result<(String, RepoStatus, RepoActionOutcome)> {
    let mut transcript = Vec::new();
    let has_remote = git_has_remote(cwd)?;
    if !has_remote {
        let status = inspect_project_without_remote_refresh(cwd);
        transcript.push("No remote configured, so there was nothing to update.".to_string());
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
            "Fetched remotes, but skipped the fast-forward because the current branch has no upstream."
                .to_string(),
        );
        return Ok((
            transcript.join("\n\n"),
            status_after_fetch,
            RepoActionOutcome::Skipped,
        ));
    }

    if matches!(mode, RepoUpdateMode::DiscardLocal)
        && (status_after_fetch.dirty || status_after_fetch.ahead > 0)
    {
        transcript
            .push("Discarding local work because you confirmed the reset to upstream.".to_string());
        transcript.push(format_git_step(
            "git reset --hard @{upstream}",
            run_git_command(cwd, &["reset", "--hard", "@{upstream}"])?,
        ));
        transcript.push(format_git_step(
            "git clean -fd",
            run_git_command(cwd, &["clean", "-fd"])?,
        ));

        let final_status = inspect_project_without_remote_refresh(cwd);
        transcript.push(format!("Final status: {}", final_status.short_label()));
        return Ok((
            transcript.join("\n\n"),
            final_status,
            RepoActionOutcome::Updated,
        ));
    }

    if status_after_fetch.dirty {
        transcript.push(
            "Fetched remotes, but skipped the fast-forward because the working tree has uncommitted changes."
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
            RepoActionOutcome::UpToDate,
        ));
    }

    if status_after_fetch.ahead > 0 {
        transcript.push(
            "Fetched remotes, but skipped the fast-forward because the branch has local commits."
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
        RepoActionOutcome::Updated,
    ))
}

fn execute_repo_update(project: &ProjectInfo, mode: RepoUpdateMode) -> RepoActionReport {
    let result = execute_github_update(&project.path, mode);

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
        state.dirty_changes_for_path = None;
    }
    rebuild_project_list(state);
}

fn render_repo_update_report(reports: &[RepoActionReport]) -> String {
    reports
        .iter()
        .map(|report| {
            format!(
                "{} [{}]\nPath: {}\nStatus: {}\n\n{}",
                report.name,
                match report.outcome {
                    RepoActionOutcome::Updated => "UPDATED".to_string(),
                    RepoActionOutcome::UpToDate => "UP TO DATE".to_string(),
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

fn summarize_repo_update(scope: RepoActionScope, reports: &[RepoActionReport]) -> String {
    let updated = reports
        .iter()
        .filter(|report| matches!(report.outcome, RepoActionOutcome::Updated))
        .count();
    let up_to_date = reports
        .iter()
        .filter(|report| matches!(report.outcome, RepoActionOutcome::UpToDate))
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
        "{} finished for {} ({updated} updated, {up_to_date} already up to date, {skipped} skipped, {failed} failed)",
        GITHUB_UPDATE_LABEL,
        describe_repo_scope_target_count(scope, reports.len()),
    )
}

fn describe_repo_scope_target_count(scope: RepoActionScope, count: usize) -> String {
    match scope {
        RepoActionScope::Current if count == 1 => "current repo".to_string(),
        RepoActionScope::Marked => format!(
            "{count} marked {}",
            if count == 1 { "repo" } else { "repos" }
        ),
        RepoActionScope::All => format!("{count} {}", if count == 1 { "repo" } else { "repos" }),
        RepoActionScope::Current => {
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

fn first_error_line(error: &anyhow::Error) -> String {
    format!("{error:#}")
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown error")
        .to_string()
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

    if let Some(binary) = profile_binary_name(spec.profile)
        && argv.first().is_some_and(|arg| arg == binary)
        && let Some(resolved_path) = resolve_binary_from_shell(binary)
    {
        argv[0] = resolved_path;
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
                    repo_path: selected_project.path.clone(),
                    cwd: session_spec.cwd.clone(),
                    title: session_spec.title(),
                });
            }

            Some(CommitPushTarget {
                repo_path: selected_project.path.clone(),
                cwd: selected_project.path.clone(),
                title: selected_project.name.clone(),
            })
        }
        None => Some(CommitPushTarget {
            repo_path: selected_project.path.clone(),
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

fn apply_repo_status_for_cwd(state: &SharedState, cwd: &Path, repo_status: RepoStatus) {
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

    {
        let mut state_mut = state.borrow_mut();
        let updated = if let Some(project) = state_mut
            .projects
            .iter_mut()
            .find(|project| project.path == repo_path)
        {
            project.repo_status = repo_status;
            true
        } else {
            false
        };
        if updated {
            state_mut.dirty_changes_for_path = None;
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

fn set_repo_action_busy(state: &SharedState, busy: bool) {
    state.borrow_mut().repo_action_busy = busy;
    update_project_ui(state);
}

fn set_refresh_busy(state: &SharedState, busy: bool) {
    state.borrow_mut().refresh_busy = busy;
    update_project_ui(state);
}

fn push_status(state: &SharedState, message: String) {
    push_status_with_details(state, message, None);
}

fn push_status_with_details(state: &SharedState, message: String, details: Option<String>) {
    let status_label = state.borrow().status_label.clone();
    status_label.set_text(&message);
    status_label.set_tooltip_text(details.as_deref());
}

fn start_status_spinner(
    state: &SharedState,
    message: String,
    details: Option<String>,
) -> glib::SourceId {
    let status_label = state.borrow().status_label.clone();
    status_label.set_tooltip_text(details.as_deref());
    state.borrow_mut().busy_frame = 0;

    let frame_index = Cell::new(0usize);
    status_label.set_text(&spinner_label(0, &message));

    let state = state.clone();
    glib::timeout_add_local(Duration::from_millis(120), move || {
        let next = (frame_index.get() + 1) % SPINNER_FRAMES.len();
        frame_index.set(next);
        {
            let mut state_mut = state.borrow_mut();
            state_mut.busy_frame = next;
        }
        status_label.set_text(&spinner_label(next, &message));
        update_project_ui(&state);
        glib::ControlFlow::Continue
    })
}

fn start_busy_button_spinner(state: &SharedState) -> glib::SourceId {
    let state = state.clone();
    glib::timeout_add_local(Duration::from_millis(120), move || {
        {
            let mut state_mut = state.borrow_mut();
            state_mut.busy_frame = (state_mut.busy_frame + 1) % SPINNER_FRAMES.len();
        }
        update_project_ui(&state);
        glib::ControlFlow::Continue
    })
}

fn start_repo_status_spinner(
    state: &SharedState,
    target_paths: Vec<PathBuf>,
    kind: RepoBusyKind,
) -> glib::SourceId {
    {
        let mut state_mut = state.borrow_mut();
        state_mut.repo_busy_feedback = Some(RepoBusyFeedback {
            kind,
            target_paths,
            frame: 0,
        });
    }
    update_project_ui(state);

    let state = state.clone();
    glib::timeout_add_local(Duration::from_millis(120), move || {
        {
            let mut state_mut = state.borrow_mut();
            let Some(feedback) = state_mut.repo_busy_feedback.as_mut() else {
                return glib::ControlFlow::Break;
            };
            feedback.frame = (feedback.frame + 1) % SPINNER_FRAMES.len();
        }
        update_project_ui(&state);
        glib::ControlFlow::Continue
    })
}

fn stop_repo_status_spinner(state: &SharedState, spinner_id: glib::SourceId) {
    spinner_id.remove();
    state.borrow_mut().repo_busy_feedback = None;
    update_project_ui(state);
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
    let font = FontDescription::from_string(
        "JetBrains Mono, DejaVu Sans Mono, Liberation Mono, Noto Mono, monospace 11",
    );
    terminal.set_font(Some(&font));

    let foreground = gdk::RGBA::parse("#d4d4d4").unwrap_or(gdk::RGBA::BLACK);
    let background = gdk::RGBA::parse("#1e1e1e").unwrap_or(gdk::RGBA::WHITE);
    let palette_values = [
        "#1e1e1e", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#e5e5e5",
        "#666666", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#ffffff",
    ];
    let palette = palette_values
        .iter()
        .map(|value| gdk::RGBA::parse(*value).unwrap_or(gdk::RGBA::BLACK))
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
        .dirty-changes-panel,
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

        .action-button:active:not(:disabled) {
            background: #094771;
            border-color: #0e639c;
            color: #ffffff;
        }

        .action-button:disabled {
            opacity: 0.45;
        }

        .busy-button,
        .busy-button:disabled {
            opacity: 1;
            background: #0e639c;
            border-color: #4fc1ff;
            color: #ffffff;
            box-shadow: inset 0 0 0 1px #4fc1ff;
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

        .dirty-changes-panel {
            padding: 8px;
        }

        .dirty-changes-title {
            color: #cccccc;
            font-size: 13px;
            font-weight: 700;
        }

        .dirty-changes-scroller {
            background: #181818;
            border: 1px solid #2d2d2d;
        }

        .dirty-changes-text {
            color: #d4d4d4;
            background: #181818;
            font-size: 12px;
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

        .targeted-project-row:not(.selected-project-row) .project-row-body {
            border-color: #6a9955;
        }

        .repo-target-check {
            margin: 0;
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

        .repo-state-busy {
            background: #094771;
            color: #ffffff;
        }

        .workspace-repo-status-busy {
            color: #ffffff;
            font-weight: 700;
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
