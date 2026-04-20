use std::{
    cell::{Cell, RefCell},
    env,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
};

use anyhow::{Context, Result, anyhow};
use gio::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CenterBox, CssProvider, DropDown,
    Entry, Grid, Label, Orientation, ResponseType, ScrolledWindow, Stack, TextBuffer, TextView,
    gdk, prelude::*,
};
use pango::FontDescription;
use vte::{CursorBlinkMode, PtyFlags, Terminal, prelude::*};

use crate::{
    persist::{Profile, SessionFile, SessionSpec, load_or_bootstrap, save},
    project::{ProjectInfo, default_roots, discover_projects},
};

const APP_ID: &str = "com.mmdmcy.BelloSaize";

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
    empty_state: GtkBox,
    project_dropdown: DropDown,
    project_model: gtk::StringList,
    status_label: Label,
    count_label: Label,
    close_button: Button,
    zoom_button: Button,
    commit_push_button: Button,
    session_file_path: PathBuf,
    project_roots: Vec<PathBuf>,
    projects: Vec<ProjectInfo>,
    sessions: Vec<Rc<SessionView>>,
    selected_session_id: Option<u64>,
    zoomed_session_id: Option<u64>,
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

fn build_ui(application: &Application) -> Result<()> {
    let cwd = env::current_dir().context("failed to resolve current working directory")?;
    let (session_file, session_file_path) = load_or_bootstrap(&cwd)?;
    let project_roots = default_roots();

    let window = ApplicationWindow::builder()
        .application(application)
        .title("BelloSaize")
        .default_width(1680)
        .default_height(960)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 16);
    root.add_css_class("app-root");
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);

    let hero = build_hero();
    root.append(&hero);

    let project_model = gtk::StringList::new(&[]);
    let project_dropdown = DropDown::builder()
        .model(&project_model)
        .hexpand(true)
        .build();
    project_dropdown.add_css_class("project-picker");

    let (toolbar_scroller, buttons) = build_toolbar(&project_dropdown);
    root.append(&toolbar_scroller);

    let grid = Grid::builder()
        .column_spacing(14)
        .row_spacing(14)
        .column_homogeneous(true)
        .row_homogeneous(true)
        .hexpand(true)
        .vexpand(true)
        .build();

    let content_scroller = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&grid)
        .build();

    let empty_state = build_empty_state();
    let stack = Stack::builder().hexpand(true).vexpand(true).build();
    stack.add_named(&empty_state, Some("empty"));
    stack.add_named(&content_scroller, Some("grid"));
    stack.set_visible_child_name("empty");
    root.append(&stack);

    let footer = build_footer();
    let status_label = footer.0;
    let count_label = footer.1;
    root.append(&footer.2);

    window.set_child(Some(&root));
    apply_css();

    let state = Rc::new(RefCell::new(AppState {
        window: window.clone(),
        stack,
        grid,
        empty_state,
        project_dropdown: project_dropdown.clone(),
        project_model,
        status_label,
        count_label,
        close_button: buttons.close_button.clone(),
        zoom_button: buttons.zoom_button.clone(),
        commit_push_button: buttons.commit_push_button.clone(),
        session_file_path,
        project_roots,
        projects: Vec::new(),
        sessions: Vec::new(),
        selected_session_id: None,
        zoomed_session_id: None,
        next_session_id: 1,
    }));

    refresh_projects(&state);

    {
        let state = state.clone();
        buttons
            .shell_button
            .connect_clicked(move |_| add_profile_session(&state, Profile::Shell));
    }
    {
        let state = state.clone();
        buttons
            .codex_button
            .connect_clicked(move |_| add_profile_session(&state, Profile::Codex));
    }
    {
        let state = state.clone();
        buttons
            .claude_button
            .connect_clicked(move |_| add_profile_session(&state, Profile::Claude));
    }
    {
        let state = state.clone();
        buttons
            .mistral_button
            .connect_clicked(move |_| add_profile_session(&state, Profile::Mistral));
    }
    {
        let state = state.clone();
        buttons
            .custom_button
            .connect_clicked(move |_| prompt_custom_session(&state));
    }
    {
        let state = state.clone();
        buttons
            .refresh_projects_button
            .connect_clicked(move |_| refresh_projects(&state));
    }
    {
        let state = state.clone();
        buttons
            .close_button
            .connect_clicked(move |_| close_selected_session(&state));
    }
    {
        let state = state.clone();
        buttons
            .zoom_button
            .connect_clicked(move |_| toggle_zoom_selected(&state));
    }
    {
        let state = state.clone();
        buttons
            .commit_push_button
            .connect_clicked(move |_| prompt_commit_and_push(&state));
    }

    {
        let state = state.clone();
        project_dropdown.connect_selected_notify(move |_| {
            let project = selected_project_path(&state)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "current working directory".to_string());
            push_status(&state, format!("launch target: {project}"));
        });
    }

    {
        let state = state.clone();
        application.connect_shutdown(move |_| {
            kill_all_sessions(&state);
            let _ = persist_sessions(&state);
        });
    }

    for spec in session_file.sessions {
        if let Err(error) = spawn_session(&state, spec) {
            push_status(&state, format!("failed to restore session: {error}"));
        }
    }

    update_layout(&state);
    window.present();
    Ok(())
}

fn build_hero() -> GtkBox {
    let hero = GtkBox::new(Orientation::Vertical, 8);
    hero.add_css_class("hero");
    hero.set_hexpand(true);
    hero.set_margin_bottom(2);

    let title = Label::new(Some("BelloSaize"));
    title.add_css_class("hero-title");
    title.set_xalign(0.0);
    title.set_margin_bottom(2);

    let subtitle = Label::new(Some(
        "Native terminal deck for Codex, Claude, Mistral, and shells. Click a pane to focus. Double-click a header to zoom.",
    ));
    subtitle.add_css_class("hero-subtitle");
    subtitle.set_wrap(true);
    subtitle.set_xalign(0.0);

    hero.append(&title);
    hero.append(&subtitle);
    hero
}

struct ToolbarButtons {
    shell_button: Button,
    codex_button: Button,
    claude_button: Button,
    mistral_button: Button,
    custom_button: Button,
    refresh_projects_button: Button,
    close_button: Button,
    zoom_button: Button,
    commit_push_button: Button,
}

fn build_toolbar(project_dropdown: &DropDown) -> (ScrolledWindow, ToolbarButtons) {
    let row = GtkBox::new(Orientation::Horizontal, 10);
    row.add_css_class("toolbar-row");

    let project_box = GtkBox::new(Orientation::Horizontal, 10);
    project_box.add_css_class("control-cluster");

    let project_label = Label::new(Some("Project"));
    project_label.add_css_class("cluster-label");
    project_box.append(&project_label);
    project_box.append(project_dropdown);

    let shell_button = large_button("New Shell");
    let codex_button = large_button("New Codex");
    let claude_button = large_button("New Claude");
    let mistral_button = large_button("New Mistral");
    let custom_button = large_button("Custom...");
    let refresh_projects_button = large_button("Reload Projects");

    let zoom_button = large_button("Zoom");
    let close_button = large_button("Close");
    let commit_push_button = large_button("Commit+Push");
    commit_push_button.add_css_class("commit-push-action");

    row.append(&project_box);
    row.append(&shell_button);
    row.append(&codex_button);
    row.append(&claude_button);
    row.append(&mistral_button);
    row.append(&custom_button);
    row.append(&refresh_projects_button);
    row.append(&zoom_button);
    row.append(&close_button);
    row.append(&commit_push_button);

    codex_button.set_sensitive(binary_exists("codex"));
    claude_button.set_sensitive(binary_exists("claude"));
    mistral_button.set_sensitive(binary_exists("mistral"));

    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(86)
        .child(&row)
        .build();
    scroller.add_css_class("toolbar-scroller");

    (
        scroller,
        ToolbarButtons {
            shell_button,
            codex_button,
            claude_button,
            mistral_button,
            custom_button,
            refresh_projects_button,
            close_button,
            zoom_button,
            commit_push_button,
        },
    )
}

fn build_empty_state() -> GtkBox {
    let box_ = GtkBox::new(Orientation::Vertical, 10);
    box_.add_css_class("empty-state");
    box_.set_valign(Align::Center);
    box_.set_halign(Align::Center);

    let title = Label::new(Some("No terminals running yet"));
    title.add_css_class("empty-title");

    let body = Label::new(Some(
        "Pick a project from the toolbar and launch a shell or agent. BelloSaize will tile the panes automatically.",
    ));
    body.add_css_class("empty-body");
    body.set_wrap(true);
    body.set_justify(gtk::Justification::Center);

    box_.append(&title);
    box_.append(&body);
    box_
}

fn build_footer() -> (Label, Label, GtkBox) {
    let footer = GtkBox::new(Orientation::Horizontal, 12);
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

fn large_button(label: &str) -> Button {
    let button = Button::with_label(label);
    button.add_css_class("large-action");
    button
}

fn refresh_projects(state: &SharedState) {
    let (previous_path, roots) = {
        let state = state.borrow();
        (
            selected_project_path_ref(&state).map(|path| path.to_string_lossy().to_string()),
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
                path: cwd,
            });
        }
    }

    let mut state_mut = state.borrow_mut();
    state_mut.projects = projects;
    let model = state_mut.project_model.clone();
    while model.n_items() > 0 {
        model.remove(0);
    }
    for project in &state_mut.projects {
        model.append(&project.name);
    }

    let selected_index = previous_path
        .and_then(|path| {
            state_mut
                .projects
                .iter()
                .position(|project| project.path.to_string_lossy() == path)
        })
        .unwrap_or(0);
    state_mut
        .project_dropdown
        .set_selected(selected_index as u32);
}

fn add_profile_session(state: &SharedState, profile: Profile) {
    let cwd = selected_project_path(state)
        .or_else(|| env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
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

#[allow(deprecated)]
fn prompt_custom_session(state: &SharedState) {
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
                    let cwd = selected_project_path(&state)
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
    if selected_session(state).is_none() {
        return;
    }

    let window = state.borrow().window.clone();
    let dialog = gtk::Dialog::builder()
        .title("Commit And Push")
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

    let entry = Entry::builder().placeholder_text("Commit message").build();
    content.append(&entry);

    {
        let state = state.clone();
        let entry = entry.clone();
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
                    run_commit_and_push_for_selected(&state, message);
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

    let badge_label = Label::new(Some(spec.profile.label()));
    badge_label.add_css_class("profile-badge");

    let status_label = Label::new(Some("LIVE"));
    status_label.add_css_class("live-pill");

    let meta_box = GtkBox::new(Orientation::Vertical, 2);
    meta_box.append(&title_label);
    meta_box.append(&subtitle_label);

    let badge_box = GtkBox::new(Orientation::Horizontal, 8);
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
    let argv = parse_command(&spec.resolved_command())?;
    let argv_strings = argv.clone();
    let workdir = spec.cwd.to_string_lossy().to_string();
    let envv = vec![
        "TERM=xterm-256color".to_string(),
        "COLORTERM=truecolor".to_string(),
        format!("SHELL={}", crate::persist::default_shell()),
    ];

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
    state_mut.close_button.set_sensitive(focused);
    state_mut.zoom_button.set_sensitive(focused);
    state_mut.commit_push_button.set_sensitive(focused);
    let _ = &state_mut.empty_state;
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

fn close_selected_session(state: &SharedState) {
    let Some(id) = state.borrow().selected_session_id else {
        return;
    };

    let removed = {
        let mut state_mut = state.borrow_mut();
        let Some(index) = state_mut
            .sessions
            .iter()
            .position(|session| session.id == id)
        else {
            return;
        };

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

    removed.card.unparent();
    if let Err(error) = persist_sessions(state) {
        show_output_dialog(
            &state.borrow().window,
            "Persistence Error",
            &format!("{error:#}"),
        );
    }
    update_layout(state);
    push_status(state, format!("closed {}", removed.spec.borrow().title()));
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

fn run_commit_and_push_for_selected(state: &SharedState, message: String) {
    let Some(session) = selected_session(state) else {
        return;
    };

    let cwd = session.spec.borrow().cwd.clone();
    let title = session.spec.borrow().title();
    let result = execute_commit_and_push(&cwd, &message);
    match result {
        Ok(output) => {
            show_output_dialog(
                &state.borrow().window,
                &format!("Commit+Push for {title}"),
                &output,
            );
            push_status(state, format!("commit+push finished in {}", cwd.display()));
        }
        Err(error) => {
            show_output_dialog(
                &state.borrow().window,
                "Commit+Push Failed",
                &format!("{error:#}"),
            );
        }
    }
}

fn execute_commit_and_push(cwd: &Path, message: &str) -> Result<String> {
    let mut transcript = Vec::new();
    transcript.push(format_git_step(
        "git add -A",
        run_git_command(cwd, &["add", "-A"])?,
    ));
    transcript.push(format_git_step(
        &format!("git commit -m {:?}", message),
        run_git_command(cwd, &["commit", "-m", message])?,
    ));

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

fn run_git_command(cwd: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command.current_dir(cwd).args(args);

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
    let (path, sessions) = {
        let state = state.borrow();
        let sessions = state
            .sessions
            .iter()
            .map(|session| session.spec.borrow().clone())
            .collect::<Vec<_>>();
        (state.session_file_path.clone(), sessions)
    };

    save(&path, &SessionFile { sessions })
}

fn parse_command(command: &str) -> Result<Vec<String>> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok(vec![crate::persist::default_shell()]);
    }

    shlex::split(trimmed).ok_or_else(|| anyhow!("invalid command line: {trimmed}"))
}

fn selected_project_path(state: &SharedState) -> Option<PathBuf> {
    let state = state.borrow();
    selected_project_path_ref(&state)
}

fn selected_project_path_ref(state: &AppState) -> Option<PathBuf> {
    state
        .projects
        .get(state.project_dropdown.selected() as usize)
        .map(|project| project.path.clone())
}

fn selected_session(state: &SharedState) -> Option<Rc<SessionView>> {
    let state = state.borrow();
    let id = state.selected_session_id?;
    state
        .sessions
        .iter()
        .find(|session| session.id == id)
        .cloned()
}

fn next_session_id(state: &SharedState) -> u64 {
    let mut state = state.borrow_mut();
    let next = state.next_session_id;
    state.next_session_id += 1;
    next
}

fn push_status(state: &SharedState, message: String) {
    state.borrow().status_label.set_text(&message);
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

fn apply_terminal_theme(terminal: &Terminal) {
    let font = FontDescription::from_string("JetBrains Mono 11");
    terminal.set_font(Some(&font));

    let foreground = gdk::RGBA::parse("#f7f3e8").unwrap_or_else(|_| gdk::RGBA::BLACK);
    let background = gdk::RGBA::parse("#111417").unwrap_or_else(|_| gdk::RGBA::WHITE);
    let palette_values = [
        "#111417", "#f86542", "#7ecf76", "#f1c76d", "#74a8ff", "#d48dff", "#67d3d7", "#f7f3e8",
        "#364049", "#ff9774", "#a4e89a", "#f7d998", "#9fc2ff", "#e6b6ff", "#9ce2e6", "#ffffff",
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
        .app-root {
            background:
                radial-gradient(circle at top left, rgba(245, 163, 114, 0.16), transparent 34%),
                radial-gradient(circle at bottom right, rgba(82, 166, 222, 0.16), transparent 30%),
                linear-gradient(180deg, #101215 0%, #0b0e10 100%);
        }

        .hero {
            padding: 18px 22px 16px 22px;
            background:
                linear-gradient(180deg, rgba(247, 243, 232, 0.08), rgba(247, 243, 232, 0.03));
            border: 1px solid rgba(247, 243, 232, 0.10);
            border-radius: 24px;
            box-shadow: 0 16px 48px rgba(0, 0, 0, 0.24);
        }

        .hero-title {
            font-size: 34px;
            font-weight: 900;
            font-family: \"JetBrains Mono\", monospace;
            color: #fff6e8;
            letter-spacing: 0.10em;
        }

        .hero-subtitle {
            color: rgba(247, 243, 232, 0.78);
            font-size: 15px;
        }

        .toolbar-scroller,
        .footer,
        .empty-state,
        .session-card {
            background: rgba(20, 24, 28, 0.88);
            border: 1px solid rgba(247, 243, 232, 0.10);
            border-radius: 22px;
            box-shadow: 0 20px 60px rgba(0, 0, 0, 0.28);
        }

        .toolbar-row {
            padding: 12px;
        }

        .control-cluster {
            min-width: 300px;
            padding: 10px 12px;
            margin-right: 8px;
            background: rgba(247, 243, 232, 0.05);
            border-radius: 18px;
        }

        .cluster-label {
            font-weight: 700;
            color: #f7f3e8;
        }

        .project-picker {
            min-width: 260px;
        }

        .large-action {
            min-height: 46px;
            padding: 0 16px;
            font-weight: 700;
            border-radius: 16px;
        }

        .large-action:not(:disabled) {
            background: linear-gradient(180deg, #efe4cf 0%, #d6c2a2 100%);
            color: #161a1e;
        }

        .commit-push-action:not(:disabled) {
            background: linear-gradient(180deg, #ffbe77 0%, #ef7240 100%);
            color: #121518;
        }

        .large-action:disabled {
            opacity: 0.45;
        }

        .empty-state {
            padding: 48px;
        }

        .empty-title {
            font-size: 30px;
            font-weight: 800;
            color: #fff6e8;
        }

        .empty-body {
            font-size: 16px;
            color: rgba(247, 243, 232, 0.75);
        }

        .footer {
            padding: 12px 16px;
        }

        .footer-status,
        .footer-count {
            color: rgba(247, 243, 232, 0.80);
            font-weight: 600;
        }

        .session-card {
            padding: 0;
        }

        .session-header {
            padding: 14px 16px;
            background:
                linear-gradient(180deg, rgba(247, 243, 232, 0.08), rgba(247, 243, 232, 0.02));
            border-bottom: 1px solid rgba(247, 243, 232, 0.10);
        }

        .selected-card {
            border-color: rgba(241, 199, 109, 0.82);
            box-shadow: 0 0 0 2px rgba(241, 199, 109, 0.16), 0 22px 70px rgba(0, 0, 0, 0.32);
        }

        .session-title {
            font-size: 18px;
            font-weight: 800;
            color: #fff6e8;
        }

        .session-subtitle {
            color: rgba(247, 243, 232, 0.70);
            font-size: 12px;
        }

        .profile-badge,
        .live-pill,
        .dead-pill {
            padding: 6px 10px;
            border-radius: 999px;
            font-size: 11px;
            font-weight: 800;
            letter-spacing: 0.06em;
        }

        .profile-badge {
            background: rgba(116, 168, 255, 0.18);
            color: #c8dcff;
        }

        .live-pill {
            background: rgba(126, 207, 118, 0.18);
            color: #cbffbe;
        }

        .dead-pill {
            background: rgba(248, 101, 66, 0.20);
            color: #ffd0c3;
        }
        ",
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
