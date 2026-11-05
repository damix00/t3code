//! The main window, laid out like T3 Code: the thread sidebar, a breadcrumb header, the chat
//! column and the composer card.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, App, ClickEvent, Context, Entity, FocusHandle, Focusable,
    FontWeight, Hsla, KeyBinding, ListAlignment, ListState, SharedString, Subscription, Task,
    Transformation, WeakEntity, Window, actions, div, list, percentage, prelude::*, px, svg,
};
use t3_contracts::{
    ApprovalDecision, Command, ProjectCreate, ProjectId, RuntimeRequestId, ThreadId,
};
use t3_state::shell::ShellStatus;
use t3_state::thread::ThreadStatus as LoadStatus;
use t3_state::transcript::{
    EntryKind, RunSummary, TodoStatus, TranscriptEntry, build_runs, build_transcript,
};
use tokio::runtime::Handle;

use crate::actions::{self as commands, ComposerTarget, Submission};
use crate::environment::{self, Environment};
use crate::sidebar::{self, SidebarModel, ThreadRow, ThreadStatus};
use crate::timeline::{self, Row, WorkIcon};
use crate::ui::assets::Icon;
use crate::ui::markdown;
use crate::ui::text_input::{TextInput, TextInputEvent};
use crate::ui::theme;
use crate::view_model::{self, Banner};

actions!(
    workspace,
    [
        NewThread,
        ConnectRemote,
        AddProject,
        CloseDialog,
        ToggleSidebar,
        Search,
        Quit
    ]
);

pub fn bindings() -> Vec<KeyBinding> {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    vec![
        KeyBinding::new(&format!("{modifier}-n"), NewThread, None),
        KeyBinding::new(&format!("{modifier}-shift-o"), AddProject, None),
        KeyBinding::new(&format!("{modifier}-shift-r"), ConnectRemote, None),
        KeyBinding::new(&format!("{modifier}-b"), ToggleSidebar, None),
        KeyBinding::new(&format!("{modifier}-k"), Search, None),
        KeyBinding::new("escape", CloseDialog, None),
        KeyBinding::new(&format!("{modifier}-q"), Quit, None),
    ]
}

const SIDEBAR_WIDTH: f32 = 256.;
const HEADER_HEIGHT: f32 = 52.;
const CHAT_WIDTH: f32 = 736.;
/// Room for the traffic lights when the sidebar is hidden.
const TRAFFIC_LIGHTS: f32 = 84.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialog {
    Pair,
    AddProject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Shelf {
    Working,
    Snoozed,
    Settled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectChoice {
    id: ProjectId,
    title: String,
}

/// What the chat list draws; rebuilt when the open thread changes.
#[derive(Default)]
struct Chat {
    entries: Vec<TranscriptEntry>,
    rows: Vec<Row>,
    /// Parsed assistant text by entry index.
    markdown: Vec<Option<Rc<Vec<crate::markdown::Block>>>>,
}

pub struct Workspace {
    runtime: Handle,
    environments: Vec<Environment>,
    active: usize,
    selected_project: Option<ProjectId>,
    projects: Vec<ProjectChoice>,
    sidebar: SidebarModel,
    sidebar_open: bool,
    open_shelves: HashSet<Shelf>,
    chat: Rc<Chat>,
    chat_thread: Option<ThreadId>,
    expanded: Rc<HashSet<String>>,
    list_state: ListState,
    composer: Entity<TextInput>,
    search: Entity<TextInput>,
    searching: bool,
    history: Vec<Option<ThreadId>>,
    history_index: usize,
    dialog: Option<Dialog>,
    dialog_input: Entity<TextInput>,
    dialog_error: Option<String>,
    notice: Option<String>,
    sending: bool,
    focus_handle: FocusHandle,
    watchers: Vec<Task<()>>,
    _ticker: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("environments", &self.environments)
            .field("active", &self.active)
            .field("selected_project", &self.selected_project)
            .finish_non_exhaustive()
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn icon(icon: Icon, size: f32, color: Hsla) -> gpui::Svg {
    svg()
        .path(icon.path())
        .size(px(size))
        .flex_none()
        .text_color(color)
}

fn spinner(id: impl Into<SharedString>, size: f32, color: Hsla) -> impl IntoElement {
    icon(Icon::LoaderCircle, size, color).with_animation(
        gpui::ElementId::Name(id.into()),
        Animation::new(Duration::from_millis(900)).repeat(),
        |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
    )
}

/// A 28px square icon button.
fn icon_button(
    id: impl Into<SharedString>,
    glyph: Icon,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .child(icon(
            glyph,
            16.,
            if enabled {
                theme::sidebar_muted()
            } else {
                theme::text_faint().opacity(0.6)
            },
        ))
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(theme::button_hover()))
        })
}

/// A labelled button for dialogs and approval cards.
fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    primary: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .h(px(32.))
        .px_3()
        .flex()
        .items_center()
        .rounded_lg()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .child(label.into())
        .map(|button| {
            if primary {
                button
                    .bg(theme::text())
                    .text_color(theme::background())
                    .hover(|style| style.bg(theme::text().opacity(0.85)))
            } else {
                button
                    .border_1()
                    .border_color(theme::input())
                    .text_color(theme::text())
                    .hover(|style| style.bg(theme::button_hover()))
            }
        })
}

/// A project's two-letter tile.
fn monogram(text: &str, color: u32) -> gpui::Div {
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(theme::tint(color, 0.14))
        .text_color(theme::tint(color, 1.))
        .font_family(theme::mono())
        .font_weight(FontWeight::BOLD)
        .text_size(px(8.25))
        .line_height(px(10.))
        .child(text.to_owned())
}

/// Text cut to one line with an ellipsis. GPUI caches the first measurement of no-wrap text, so
/// this clamps wrapped text instead, which is re-measured once its width is known.
fn one_line(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .text_ellipsis()
        .line_clamp(1)
        .child(text.into())
}

/// Double-clicking the title area zooms the window, as on any macOS title bar.
fn title_bar_double_click(event: &ClickEvent, window: &mut Window, _: &mut App) {
    if event.click_count() == 2 && cfg!(target_os = "macos") {
        window.titlebar_double_click();
    }
}

impl Workspace {
    pub fn new(
        runtime: Handle,
        environments: Vec<Environment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| TextInput::new("Ask anything", cx));
        let search = cx.new(|cx| TextInput::new("Search", cx));
        let dialog_input = cx.new(|cx| TextInput::new("", cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event, window, cx| match event {
                    TextInputEvent::Submit => this.submit(window, cx),
                    TextInputEvent::Changed => cx.notify(),
                },
            ),
            cx.subscribe_in(&search, window, |_, _, _, _, cx| cx.notify()),
            cx.subscribe_in(&dialog_input, window, |this, _, event, window, cx| {
                if *event == TextInputEvent::Submit {
                    this.confirm_dialog(window, cx);
                }
            }),
        ];
        // Relative times and "Working 8m" labels age even when nothing syncs.
        let ticker = cx.spawn(async move |this: WeakEntity<Self>, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        });
        let mut workspace = Self {
            runtime,
            environments,
            active: 0,
            selected_project: None,
            projects: Vec::new(),
            sidebar: SidebarModel::default(),
            sidebar_open: true,
            open_shelves: HashSet::from([Shelf::Working]),
            chat: Rc::new(Chat::default()),
            chat_thread: None,
            expanded: Rc::new(HashSet::new()),
            list_state: ListState::new(0, ListAlignment::Bottom, px(800.)),
            composer,
            search,
            searching: false,
            history: vec![None],
            history_index: 0,
            dialog: None,
            dialog_input,
            dialog_error: None,
            notice: None,
            sending: false,
            focus_handle: cx.focus_handle(),
            watchers: Vec::new(),
            _ticker: ticker,
            _subscriptions: subscriptions,
        };
        workspace.watch_active(cx);
        if workspace.environments.is_empty() {
            workspace.open_dialog(Dialog::Pair, window, cx);
        }
        workspace.refresh(cx);
        workspace
    }

    /// Opens a thread given on the command line.
    pub fn open_initial_thread(
        &mut self,
        thread: ThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(Some(thread), window, cx);
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.composer.focus_handle(cx));
    }

    fn active(&self) -> Option<&Environment> {
        self.environments.get(self.active)
    }

    /// Re-reads state whenever the active environment or its local server changes, at most
    /// once a frame.
    fn watch_active(&mut self, cx: &mut Context<Self>) {
        self.watchers.clear();
        let Some(environment) = self.active() else {
            return;
        };
        let mut revision = environment.sync.subscribe();
        let backend_status = environment.backend_status();
        self.watchers
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx| {
                loop {
                    if revision.changed().await.is_err() {
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(16))
                        .await;
                    revision.borrow_and_update();
                    if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                        break;
                    }
                }
            }));
        if let Some(mut status) = backend_status {
            self.watchers
                .push(cx.spawn(async move |this: WeakEntity<Self>, cx| {
                    while status.changed().await.is_ok() {
                        if this.update(cx, |_, cx| cx.notify()).is_err() {
                            break;
                        }
                    }
                }));
        }
    }

    fn build_sidebar(&self) -> (SidebarModel, Vec<ProjectChoice>) {
        let Some(environment) = self.active() else {
            return Default::default();
        };
        let now = now_ms();
        environment.sync.read(|state| {
            let Some(snapshot) = state.shell.snapshot() else {
                return Default::default();
            };
            let projects = snapshot
                .projects
                .iter()
                .filter(|project| !project.is_archived())
                .map(|project| ProjectChoice {
                    id: project.id.clone(),
                    title: project.title.clone(),
                })
                .collect();
            (sidebar::build(snapshot, now), projects)
        })
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let (sidebar, _) = self.build_sidebar();
        if sidebar != self.sidebar {
            self.sidebar = sidebar;
            cx.notify();
        }
    }

    /// Rebuilds the sidebar and chat from sync state.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (sidebar, projects) = self.build_sidebar();
        self.sidebar = sidebar;
        self.projects = projects;
        if self
            .selected_project
            .as_ref()
            .is_none_or(|id| !self.projects.iter().any(|project| &project.id == id))
        {
            self.selected_project = self.projects.first().map(|project| project.id.clone());
        }
        let (open, entries, runs) = self
            .active()
            .map(|environment| {
                environment.sync.read(|state| {
                    let open = state.thread.as_ref().map(|open| open.id.clone());
                    let projection = state
                        .thread
                        .as_ref()
                        .and_then(|open| open.state.projection());
                    (
                        open,
                        projection.map(build_transcript).unwrap_or_default(),
                        projection.map(build_runs).unwrap_or_default(),
                    )
                })
            })
            .unwrap_or_default();
        if let Some(project) = open
            .as_ref()
            .and_then(|id| self.sidebar.find(id))
            .map(|row| row.project_id.clone())
        {
            self.selected_project = Some(project);
        }
        self.set_chat(open, entries, &runs);
        self.update_placeholder(cx);
        cx.notify();
    }

    fn set_chat(
        &mut self,
        thread: Option<ThreadId>,
        entries: Vec<TranscriptEntry>,
        runs: &[RunSummary],
    ) {
        let rows = timeline::build_rows(&entries, runs);
        let old = Rc::clone(&self.chat);
        let markdown = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| match &entry.kind {
                EntryKind::Assistant { text, .. } | EntryKind::Plan { markdown: text, .. } => {
                    // Reuse the parse when the text has not changed.
                    match (old.entries.get(index), old.markdown.get(index)) {
                        (Some(previous), Some(Some(parsed))) if previous.kind == entry.kind => {
                            Some(Rc::clone(parsed))
                        }
                        _ => Some(Rc::new(crate::markdown::parse(text))),
                    }
                }
                _ => None,
            })
            .collect();
        if thread != self.chat_thread {
            self.list_state.reset(rows.len());
            self.chat_thread = thread;
            self.expanded = Rc::new(HashSet::new());
        } else if let Some(first) = first_changed_row(&old, &rows, &entries) {
            self.list_state
                .splice(first..old.rows.len(), rows.len().saturating_sub(first));
        }
        self.chat = Rc::new(Chat {
            entries,
            rows,
            markdown,
        });
    }

    fn open_thread_id(&self) -> Option<ThreadId> {
        self.active()?
            .sync
            .read(|state| state.thread.as_ref().map(|open| open.id.clone()))
    }

    fn composer_target(&self) -> ComposerTarget {
        view_model::composer_target(
            self.open_thread_id().as_ref(),
            self.selected_project.as_ref(),
        )
    }

    fn update_placeholder(&mut self, cx: &mut Context<Self>) {
        let placeholder = view_model::composer_placeholder(&self.composer_target());
        self.composer
            .update(cx, |composer, cx| composer.set_placeholder(placeholder, cx));
    }

    fn open(&mut self, thread: Option<ThreadId>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(environment) = self.active() {
            environment.sync.open_thread(thread);
        }
        self.notice = None;
        self.focus_composer(window, cx);
        self.refresh(cx);
    }

    /// Opens a thread (or the new-thread view) and records it for back and forward.
    fn navigate(&mut self, thread: Option<ThreadId>, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.get(self.history_index) != Some(&thread) {
            self.history.truncate(self.history_index + 1);
            self.history.push(thread.clone());
            self.history_index = self.history.len() - 1;
        }
        self.open(thread, window, cx);
    }

    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_index > 0 {
            self.history_index -= 1;
            let thread = self.history[self.history_index].clone();
            self.open(thread, window, cx);
        }
    }

    fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_index + 1 < self.history.len() {
            self.history_index += 1;
            let thread = self.history[self.history_index].clone();
            self.open(thread, window, cx);
        }
    }

    fn start_new_thread(
        &mut self,
        project: Option<ProjectId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = project {
            self.selected_project = Some(project);
        }
        self.navigate(None, window, cx);
    }

    fn switch_environment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == self.active || index >= self.environments.len() {
            return;
        }
        self.active = index;
        self.selected_project = None;
        self.notice = None;
        self.history = vec![None];
        self.history_index = 0;
        self.watch_active(cx);
        self.refresh(cx);
    }

    fn show_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(message.into());
        cx.notify();
    }

    fn toggle_expanded(&mut self, key: String, index: usize, cx: &mut Context<Self>) {
        let mut expanded = (*self.expanded).clone();
        if !expanded.remove(&key) {
            expanded.insert(key);
        }
        self.expanded = Rc::new(expanded);
        self.list_state.splice(index..index + 1, 1);
        cx.notify();
    }

    fn toggle_shelf(&mut self, shelf: Shelf, cx: &mut Context<Self>) {
        if !self.open_shelves.remove(&shelf) {
            self.open_shelves.insert(shelf);
        }
        cx.notify();
    }

    fn start_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = true;
        self.searching = true;
        window.focus(&self.search.focus_handle(cx));
        cx.notify();
    }

    fn end_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.searching = false;
        self.search.update(cx, |search, cx| search.set_text("", cx));
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Runs a request on the active connection and hands the result back on the UI thread.
    fn request<T: 'static>(
        &mut self,
        cx: &mut Context<Self>,
        start: impl FnOnce(
            t3_client::Connection,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>>>>,
        done: impl FnOnce(&mut Self, Result<T, String>, &mut Context<Self>) + 'static,
    ) {
        let Some(connection) = self
            .active()
            .and_then(|environment| environment.sync.connection())
        else {
            done(
                self,
                Err("Not connected yet. Try again in a moment.".into()),
                cx,
            );
            return;
        };
        let future = start(connection);
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = future.await;
            let _ = this.update(cx, |this, cx| done(this, result, cx));
        })
        .detach();
    }

    fn dispatch(&mut self, command: Command, cx: &mut Context<Self>) {
        self.request(
            cx,
            move |connection| {
                Box::pin(async move {
                    connection
                        .dispatch_command(&command)
                        .await
                        .map_err(|e| e.to_string())
                })
            },
            |this, result, cx| {
                if let Err(error) = result {
                    this.show_error(error, cx);
                }
            },
        );
    }

    fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        let text = self.composer.read(cx).text().to_owned();
        let config = self
            .active()
            .and_then(|environment| environment.sync.read(|state| state.config.clone()));
        let submission = match commands::submission(&self.composer_target(), &text, config.as_ref())
        {
            Ok(submission) => submission,
            Err(commands::SubmitError::Empty) => return,
            Err(error) => return self.show_error(error.to_string(), cx),
        };
        self.sending = true;
        self.composer
            .update(cx, |composer, cx| composer.set_disabled(true, cx));
        let finish = move |this: &mut Self,
                           result: Result<Option<ThreadId>, String>,
                           cx: &mut Context<Self>| {
            this.sending = false;
            this.composer
                .update(cx, |composer, cx| composer.set_disabled(false, cx));
            match result {
                Ok(launched) => {
                    this.composer.update(cx, |composer, cx| {
                        composer.take(cx);
                    });
                    this.notice = None;
                    if let Some(thread) = launched {
                        if let Some(environment) = this.active() {
                            environment.sync.open_thread(Some(thread.clone()));
                        }
                        this.history.truncate(this.history_index + 1);
                        this.history.push(Some(thread));
                        this.history_index = this.history.len() - 1;
                    }
                    this.refresh(cx);
                }
                Err(error) => this.show_error(error, cx),
            }
        };
        match submission {
            Submission::Dispatch(command) => self.request(
                cx,
                move |connection| {
                    Box::pin(async move {
                        connection
                            .dispatch_command(&command)
                            .await
                            .map(|_| None)
                            .map_err(|e| e.to_string())
                    })
                },
                finish,
            ),
            Submission::Launch(input) => self.request(
                cx,
                move |connection| {
                    Box::pin(async move {
                        connection
                            .launch_thread(&input)
                            .await
                            .map(|result| Some(result.thread_id))
                            .map_err(|e| e.to_string())
                    })
                },
                finish,
            ),
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(environment) = self.active() else {
            return;
        };
        let command = environment.sync.read(|state| {
            let open = state.thread.as_ref()?;
            commands::stop(&open.id, state.config.as_ref(), open.state.projection())
        });
        match command {
            Some(command) => self.dispatch(command, cx),
            None => self.show_error("Nothing is running in this thread.", cx),
        }
    }

    fn respond(
        &mut self,
        request_id: RuntimeRequestId,
        decision: ApprovalDecision,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.open_thread_id() else {
            return;
        };
        self.dispatch(commands::respond(&thread, &request_id, decision), cx);
    }

    fn open_dialog(&mut self, dialog: Dialog, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = Some(dialog);
        self.dialog_error = None;
        let placeholder = match dialog {
            Dialog::Pair => "https://host/pair#token=…  or  t3code://pair?…",
            Dialog::AddProject => "/path/to/project",
        };
        self.dialog_input.update(cx, |input, cx| {
            input.set_text("", cx);
            input.set_placeholder(placeholder, cx);
        });
        window.focus(&self.dialog_input.focus_handle(cx));
        cx.notify();
    }

    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
        } else if self.searching {
            self.end_search(window, cx);
        }
    }

    fn confirm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.dialog_input.read(cx).text().trim().to_owned();
        match self.dialog {
            None => {}
            Some(Dialog::Pair) => match environment::start_remote(&value, &self.runtime) {
                Ok(environment) => {
                    self.environments.push(environment);
                    let index = self.environments.len() - 1;
                    self.dialog = None;
                    self.switch_environment(index, cx);
                    self.focus_composer(window, cx);
                }
                Err(error) => {
                    self.dialog_error = Some(error);
                    cx.notify();
                }
            },
            Some(Dialog::AddProject) => {
                if value.is_empty() {
                    self.dialog_error = Some("Enter a folder path.".into());
                    cx.notify();
                    return;
                }
                let command = ProjectCreate::for_path(&value);
                let project_id = command.project_id.clone();
                self.request(
                    cx,
                    move |connection| {
                        Box::pin(async move {
                            connection
                                .create_project(&command)
                                .await
                                .map_err(|e| e.to_string())
                        })
                    },
                    move |this, result, cx| match result {
                        Ok(_) => {
                            this.dialog = None;
                            this.selected_project = Some(project_id);
                            if let Some(environment) = this.active() {
                                environment.sync.open_thread(None);
                            }
                            this.refresh(cx);
                        }
                        Err(error) => {
                            this.dialog_error = Some(error);
                            cx.notify();
                        }
                    },
                );
            }
        }
    }

    /// Stops every environment. Called when the app quits.
    pub fn shutdown(&mut self) {
        for environment in &self.environments {
            environment.shutdown(&self.runtime);
        }
    }

    fn selected_project_title(&self) -> Option<&str> {
        let id = self.selected_project.as_ref()?;
        self.projects
            .iter()
            .find(|project| &project.id == id)
            .map(|project| project.title.as_str())
    }

    // ---- Sidebar ----

    fn render_sidebar_top(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let can_back = self.history_index > 0;
        let can_forward = self.history_index + 1 < self.history.len();
        div()
            .id("sidebar-top")
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .pl(px(TRAFFIC_LIGHTS + 4.))
            .pr_2()
            .gap_1()
            .on_click(title_bar_double_click)
            .child(
                icon_button("toggle-sidebar", Icon::PanelLeft, true).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.sidebar_open = false;
                        cx.notify();
                    },
                )),
            )
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(4.))
                    .px_1()
                    .text_size(px(14.))
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::text())
                            .child("T3"),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_muted())
                            .child("Code"),
                    ),
            )
            .child(
                icon_button("back", Icon::ArrowLeft, can_back).when(can_back, |button| {
                    button.on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.go_back(window, cx)),
                    )
                }),
            )
            .child(icon_button("forward", Icon::ArrowRight, can_forward).when(
                can_forward,
                |button| {
                    button.on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.go_forward(window, cx)),
                    )
                },
            ))
    }

    fn render_search_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let new_project = self.selected_project.clone();
        div()
            .h(px(32.))
            .mx_2()
            .mb_2()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .id("search")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(self.searching, |row| row.bg(theme::row_hover()))
                    .hover(|style| style.bg(theme::row_hover()))
                    .child(icon(Icon::Search, 16., theme::sidebar_muted()))
                    .child(if self.searching {
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .text_color(theme::text())
                            .child(self.search.clone())
                            .into_any_element()
                    } else {
                        div()
                            .text_size(px(14.))
                            .text_color(theme::sidebar_muted())
                            .child("Search")
                            .into_any_element()
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        if !this.searching {
                            this.start_search(window, cx)
                        }
                    })),
            )
            .child(
                icon_button("add-project", Icon::FolderPlus, true).on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| {
                        this.open_dialog(Dialog::AddProject, window, cx)
                    },
                )),
            )
            .child(
                icon_button("new-thread", Icon::SquarePen, true).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.start_new_thread(new_project.clone(), window, cx)
                    },
                )),
            )
    }

    fn render_card(
        &self,
        row: &ThreadRow,
        open: Option<&ThreadId>,
        receded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_open = open == Some(&row.id);
        let id = row.id.clone();
        let trailing: AnyElement = match row.status {
            ThreadStatus::Working => div()
                .flex()
                .items_center()
                .gap_1()
                .text_color(theme::info())
                .child(spinner(format!("spin-{}", row.id), 12., theme::info()))
                .child(match &row.working_for {
                    Some(elapsed) if elapsed != "just now" => format!("Working {elapsed}"),
                    _ => "Working".to_owned(),
                })
                .into_any_element(),
            ThreadStatus::Approval => status_label("Needs approval", theme::warning()),
            ThreadStatus::Input => status_label("Needs input", theme::warning()),
            ThreadStatus::Failed => status_label("Failed", theme::danger()),
            ThreadStatus::Limited => status_label("Usage limit", theme::warning()),
            ThreadStatus::Ready => div()
                .flex()
                .items_center()
                .gap_1p5()
                .when(row.unread, |time| {
                    time.child(div().size(px(6.)).rounded_full().bg(theme::info()))
                })
                .child(row.time.clone().unwrap_or_default())
                .into_any_element(),
        };
        let details = row.branch.is_some() || row.pull_request.is_some();
        div()
            .id(SharedString::from(format!("thread-{}", row.id)))
            .mx_2()
            .px_2p5()
            .py_2()
            .flex()
            .flex_col()
            .gap(px(3.))
            .rounded_lg()
            .cursor_pointer()
            .when(is_open, |card| card.bg(theme::row_selected()))
            .when(!is_open, |card| {
                card.hover(|style| style.bg(theme::row_hover()))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .child(monogram(&row.monogram, row.color))
                    .child(
                        one_line(row.project.clone())
                            .flex_1()
                            .text_color(theme::sidebar_muted()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme::sidebar_muted())
                            .child(trailing),
                    ),
            )
            .child(
                div().flex().w_full().child(
                    one_line(row.title.clone())
                        .flex_1()
                        .text_size(px(14.))
                        .line_height(px(20.))
                        .when(receded, |title| {
                            title
                                .font_weight(FontWeight::NORMAL)
                                .text_color(theme::text().opacity(0.75))
                        })
                        .when(!receded, |title| {
                            title
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::text())
                        }),
                ),
            )
            .when(details, |card| {
                card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(theme::sidebar_muted())
                        .child(one_line(row.branch.clone().unwrap_or_default()).flex_1())
                        .children(row.pull_request.map(|number| {
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(px(2.))
                                .child(icon(Icon::GitPullRequest, 12., theme::sidebar_muted()))
                                .child(number.to_string())
                        })),
                )
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.navigate(Some(id.clone()), window, cx)
            }))
            .into_any_element()
    }

    fn render_shelf(
        &self,
        shelf: Shelf,
        label: &'static str,
        rows: &[ThreadRow],
        open: Option<&ThreadId>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if rows.is_empty() {
            return None;
        }
        let expanded = self.open_shelves.contains(&shelf);
        let receded = shelf != Shelf::Working;
        Some(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id(label)
                        .h(px(32.))
                        .mx_2()
                        .px_2p5()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::row_hover()))
                        .text_size(px(12.))
                        .text_color(theme::text_muted())
                        .child(div().flex_1().font_weight(FontWeight::MEDIUM).child(label))
                        .child(rows.len().to_string())
                        .child(icon(
                            if expanded {
                                Icon::ChevronDown
                            } else {
                                Icon::ChevronRight
                            },
                            14.,
                            theme::text_muted(),
                        ))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_shelf(shelf, cx)
                        })),
                )
                .when(expanded, |section| {
                    section.children(
                        rows.iter()
                            .map(|row| self.render_card(row, open, receded, cx))
                            .collect::<Vec<_>>(),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_environment_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let environments: Vec<(usize, String)> = self
            .environments
            .iter()
            .enumerate()
            .map(|(index, environment)| {
                let label = environment.sync.read(|state| {
                    state
                        .config
                        .as_ref()
                        .and_then(|config| config.environment_label().map(str::to_owned))
                });
                (
                    index,
                    view_model::environment_name(label.as_deref(), environment.is_local()),
                )
            })
            .collect();
        let multiple = environments.len() > 1;
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_2()
            .children(environments.into_iter().map(|(index, label)| {
                let active = index == self.active;
                div()
                    .id(("environment", index))
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .text_size(px(12.))
                    .text_color(if active {
                        theme::text()
                    } else {
                        theme::sidebar_muted()
                    })
                    .when(active && multiple, |row| row.bg(theme::row_hover()))
                    .when(multiple, |row| {
                        row.cursor_pointer()
                            .hover(|style| style.bg(theme::row_hover()))
                    })
                    .child(icon(Icon::Server, 14., theme::sidebar_muted()))
                    .child(one_line(label).flex_1())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.switch_environment(index, cx)
                    }))
            }))
            .child(
                div()
                    .id("connect-remote")
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .text_size(px(12.))
                    .text_color(theme::sidebar_muted())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::row_hover()))
                    .child(icon(Icon::Plus, 14., theme::sidebar_muted()))
                    .child("Connect remote environment")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_dialog(Dialog::Pair, window, cx)
                    })),
            )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.open_thread_id();
        let query = self.search.read(cx).text().trim().to_owned();
        let live = self.active().is_some_and(|environment| {
            environment.sync.read(|state| state.shell.status()) == ShellStatus::Live
        });
        let main: Vec<AnyElement> = if !query.is_empty() {
            let results = self.sidebar.search(&query);
            if results.is_empty() {
                vec![sidebar_hint("No matching threads")]
            } else {
                results
                    .into_iter()
                    .map(|row| self.render_card(row, open.as_ref(), false, cx))
                    .collect()
            }
        } else {
            let mut cards: Vec<AnyElement> = self
                .sidebar
                .pinned
                .iter()
                .chain(&self.sidebar.active)
                .map(|row| self.render_card(row, open.as_ref(), false, cx))
                .collect();
            if cards.is_empty() && live {
                cards.push(sidebar_hint(if self.projects.is_empty() {
                    "Add a project to get started"
                } else {
                    "No threads need you"
                }));
            }
            cards
        };
        let shelves: Vec<AnyElement> = if query.is_empty() {
            [
                self.render_shelf(
                    Shelf::Working,
                    "Working",
                    &self.sidebar.working,
                    open.as_ref(),
                    cx,
                ),
                self.render_shelf(
                    Shelf::Snoozed,
                    "Snoozed",
                    &self.sidebar.snoozed,
                    open.as_ref(),
                    cx,
                ),
                self.render_shelf(
                    Shelf::Settled,
                    "Settled",
                    &self.sidebar.settled,
                    open.as_ref(),
                    cx,
                ),
            ]
            .into_iter()
            .flatten()
            .collect()
        } else {
            Vec::new()
        };
        div()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::sidebar())
            .border_r_1()
            .border_color(theme::sidebar_border())
            .child(self.render_sidebar_top(cx))
            .child(self.render_search_row(cx))
            .child(
                div()
                    .id("threads")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .pb_2()
                    .children(main),
            )
            .when(!shelves.is_empty(), |sidebar| {
                sidebar.child(
                    div()
                        .id("shelves")
                        .flex_none()
                        .max_h(gpui::relative(0.55))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .py_1()
                        .children(shelves),
                )
            })
            .child(self.render_environment_bar(cx))
    }

    // ---- Main column ----

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.open_thread_id();
        let row = open.as_ref().and_then(|id| self.sidebar.find(id));
        let (project, color, monogram_text) = match row {
            Some(row) => (Some(row.project.clone()), row.color, row.monogram.clone()),
            None => {
                let title = self.selected_project_title().map(str::to_owned);
                let name = title.clone().unwrap_or_default();
                (
                    title,
                    sidebar::monogram_color(&name),
                    sidebar::monogram(&name),
                )
            }
        };
        let title = match (&open, row) {
            (Some(_), Some(row)) => row.title.clone(),
            (Some(_), None) => self
                .active()
                .and_then(|environment| {
                    environment.sync.read(|state| {
                        state
                            .thread
                            .as_ref()?
                            .state
                            .projection()?
                            .title()
                            .map(str::to_owned)
                    })
                })
                .unwrap_or_default(),
            (None, _) => "New thread".to_owned(),
        };
        let status = self.active().and_then(|environment| {
            environment.sync.read(|state| {
                let open = state.thread.as_ref()?;
                match (open.state.status(), &open.error) {
                    (_, Some(error)) => Some(format!("Could not load this thread: {error}")),
                    (LoadStatus::Loading, None) if open.state.projection().is_none() => {
                        Some("Loading…".into())
                    }
                    _ => None,
                }
            })
        });
        div()
            .id("header")
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .pl(px(if self.sidebar_open {
                20.
            } else {
                TRAFFIC_LIGHTS
            }))
            .pr_3()
            .on_click(title_bar_double_click)
            .when(!self.sidebar_open, |header| {
                header.child(icon_button("show-sidebar", Icon::PanelLeft, true).on_click(
                    cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.sidebar_open = true;
                        cx.notify();
                    }),
                ))
            })
            .when_some(project, |header, project| {
                header
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap_1p5()
                            .child(monogram(&monogram_text, color))
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text_muted())
                                    .child(project),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(theme::text_faint())
                            .child("/"),
                    )
            })
            .child(
                one_line(title)
                    .text_size(px(14.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text()),
            )
            .children(status.map(|status| {
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(theme::text_muted())
                    .child(status)
            }))
    }

    fn render_banner(&self) -> Option<impl IntoElement + use<>> {
        let environment = self.active()?;
        let connection = environment.sync.read(|state| state.status.clone());
        let backend = environment
            .backend_status()
            .map(|status| status.borrow().clone());
        let Banner { tone, text } = view_model::connection_banner(&connection, backend.as_ref())?;
        Some(
            div()
                .flex_none()
                .flex()
                .justify_center()
                .px_5()
                .pb_2()
                .child(
                    div()
                        .w_full()
                        .max_w(px(CHAT_WIDTH))
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::tone(tone).opacity(0.3))
                        .bg(theme::tone(tone).opacity(0.08))
                        .text_size(px(13.))
                        .text_color(theme::tone(tone))
                        .child(text),
                ),
        )
    }

    fn working(&self) -> bool {
        self.open_thread_id()
            .and_then(|id| self.sidebar.find(&id))
            .is_some_and(|row| row.status == ThreadStatus::Working)
    }

    fn render_chat(&self, cx: &mut Context<Self>) -> AnyElement {
        let chat = Rc::clone(&self.chat);
        let expanded = Rc::clone(&self.expanded);
        let workspace = cx.weak_entity();
        list(self.list_state.clone(), move |index, _, _| {
            let Some(row) = chat.rows.get(index) else {
                return div().into_any_element();
            };
            let context = RowContext {
                chat: &chat,
                expanded: &expanded,
                workspace: &workspace,
                index,
            };
            let first = index == 0;
            let last = index + 1 == chat.rows.len();
            div()
                .w_full()
                .flex()
                .justify_center()
                .px_5()
                .when(first, |row| row.pt_4())
                .when(last, |row| row.pb_6())
                .child(
                    div()
                        .w_full()
                        .max_w(px(CHAT_WIDTH))
                        .px_1()
                        .py(px(6.))
                        .child(render_row(row, &context)),
                )
                .into_any_element()
        })
        .flex_1()
        .w_full()
        .into_any_element()
    }

    fn model_label(&self) -> Option<(String, Option<String>)> {
        let environment = self.active()?;
        environment.sync.read(|state| {
            let selection = match state.thread.as_ref() {
                Some(open) => view_model::find_thread(state.shell.snapshot(), &open.id)
                    .map(|thread| thread.model_selection.clone()),
                None => state
                    .config
                    .as_ref()
                    .and_then(t3_contracts::ServerConfig::default_model_selection),
            }?;
            Some(view_model::model_label(&selection))
        })
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let working = self.working();
        let can_send = !self.sending && !self.composer.read(cx).is_blank();
        let action: AnyElement = if working && !can_send {
            div()
                .id("stop")
                .size(px(32.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme::destructive())
                .cursor_pointer()
                .hover(|style| style.bg(theme::destructive().opacity(0.85)))
                .child(icon(Icon::Square, 12., theme::text()))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop(cx)))
                .into_any_element()
        } else {
            div()
                .id("send")
                .size(px(32.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .map(|button| {
                    if can_send {
                        button
                            .bg(theme::text())
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::text().opacity(0.85)))
                    } else {
                        button.bg(theme::input())
                    }
                })
                .child(icon(
                    Icon::ArrowUp,
                    16.,
                    if can_send {
                        theme::background()
                    } else {
                        theme::text_faint()
                    },
                ))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.submit(window, cx)))
                .into_any_element()
        };
        let model = self.model_label();
        div()
            .w_full()
            .max_w(px(CHAT_WIDTH))
            .flex()
            .flex_col()
            .rounded(px(16.))
            .border_1()
            .border_color(theme::composer_border())
            .bg(theme::composer())
            .child(
                div()
                    .id("composer-scroll")
                    .min_h(px(44.))
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .px_4()
                    .pt(px(14.))
                    .text_size(px(14.))
                    .line_height(px(22.))
                    .text_color(theme::text())
                    .child(self.composer.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_3()
                    .px_3()
                    .pb_3()
                    .pt_2()
                    .when_some(model, |row, (name, effort)| {
                        row.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_size(px(14.))
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme::text())
                                        .child(name),
                                )
                                .children(effort.map(|effort| {
                                    div().text_color(theme::text_muted()).child(effort)
                                })),
                        )
                    })
                    .child(action),
            )
    }

    fn render_notice(&self) -> Option<impl IntoElement + use<>> {
        let notice = self.notice.clone()?;
        Some(
            div().flex().justify_center().px_5().pb_2().child(
                div()
                    .w_full()
                    .max_w(px(CHAT_WIDTH))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(13.))
                    .text_color(theme::danger())
                    .child(icon(Icon::CircleAlert, 14., theme::danger()))
                    .child(notice),
            ),
        )
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let heading = match self.selected_project_title() {
            Some(title) => format!("What should we build in {title}?"),
            None => "Add a project to get started".to_owned(),
        };
        let selected = self.selected_project.clone();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_6()
            .px_5()
            .pb(px(HEADER_HEIGHT))
            .child(
                div()
                    .text_size(px(28.))
                    .line_height(px(36.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .child(heading),
            )
            .when(self.selected_project.is_some(), |empty| {
                empty.child(self.render_composer(cx))
            })
            .children(self.render_notice())
            .child(
                div()
                    .w_full()
                    .max_w(px(CHAT_WIDTH))
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap_2()
                    .children(self.projects.iter().map(|project| {
                        let id = project.id.clone();
                        let chosen = selected.as_ref() == Some(&project.id);
                        let color = sidebar::monogram_color(&project.title);
                        div()
                            .id(SharedString::from(format!("project-{}", project.id)))
                            .h(px(28.))
                            .px_2p5()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .rounded_full()
                            .border_1()
                            .border_color(if chosen {
                                theme::input()
                            } else {
                                theme::border()
                            })
                            .when(chosen, |chip| chip.bg(theme::row_hover()))
                            .text_size(px(12.))
                            .text_color(if chosen {
                                theme::text()
                            } else {
                                theme::text_muted()
                            })
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::row_hover()))
                            .child(monogram(&sidebar::monogram(&project.title), color))
                            .child(project.title.clone())
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.selected_project = Some(id.clone());
                                this.update_placeholder(cx);
                                this.focus_composer(window, cx);
                                cx.notify();
                            }))
                    }))
                    .when(self.projects.is_empty(), |chips| {
                        chips.child(button("empty-add-project", "Add project…", true).on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.open_dialog(Dialog::AddProject, window, cx)
                            }),
                        ))
                    }),
            )
    }

    fn render_main(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let thread_open = self.open_thread_id().is_some();
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::background())
            .child(self.render_header(cx))
            .children(self.render_banner())
            .map(|main| {
                if thread_open {
                    main.child(self.render_chat(cx))
                        .children(self.render_notice())
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .justify_center()
                                .px_5()
                                .pb_5()
                                .child(self.render_composer(cx)),
                        )
                } else {
                    main.child(self.render_empty_state(cx))
                }
            })
    }

    fn render_dialog(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let dialog = self.dialog?;
        let (title, hint, confirm) = match dialog {
            Dialog::Pair => (
                "Connect a remote environment",
                "Paste a pairing link from T3 Code on the other machine (Settings → Connections).",
                "Connect",
            ),
            Dialog::AddProject => (
                "Add project",
                "A folder on the environment's machine. It is created if it does not exist.",
                "Add",
            ),
        };
        Some(
            div()
                .id("dialog-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::scrim())
                .on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.close_dialog(window, cx)),
                )
                .child(
                    div()
                        .id("dialog")
                        .w(px(480.))
                        .p_5()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded_xl()
                        .border_1()
                        .border_color(theme::input())
                        .bg(theme::card())
                        // Clicks inside the dialog do not dismiss it.
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .text_size(px(16.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(theme::text_muted())
                                .child(hint),
                        )
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded_lg()
                                .border_1()
                                .border_color(theme::input())
                                .bg(theme::background())
                                .text_size(px(14.))
                                .line_height(px(20.))
                                .child(self.dialog_input.clone()),
                        )
                        .children(self.dialog_error.clone().map(|error| {
                            div()
                                .text_size(px(13.))
                                .text_color(theme::danger())
                                .child(error)
                        }))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .pt_1()
                                .child(button("dialog-cancel", "Cancel", false).on_click(
                                    cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.close_dialog(window, cx)
                                    }),
                                ))
                                .child(button("dialog-confirm", confirm, true).on_click(
                                    cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.confirm_dialog(window, cx)
                                    }),
                                )),
                        ),
                ),
        )
    }
}

fn status_label(text: &'static str, color: Hsla) -> AnyElement {
    div().text_color(color).child(text).into_any_element()
}

fn sidebar_hint(text: &'static str) -> AnyElement {
    div()
        .px_4()
        .py_2()
        .text_size(px(13.))
        .text_color(theme::sidebar_muted())
        .child(text)
        .into_any_element()
}

/// The first row the list must re-measure: where the rows differ, or where an entry they draw
/// changed.
fn first_changed_row(old: &Chat, rows: &[Row], entries: &[TranscriptEntry]) -> Option<usize> {
    let by_row = old
        .rows
        .iter()
        .zip(rows)
        .position(|(old, new)| old != new)
        .or_else(|| (old.rows.len() != rows.len()).then(|| old.rows.len().min(rows.len())));
    let by_entry = view_model::first_change(&old.entries, entries)
        .and_then(|entry| rows.iter().position(|row| max_entry(row) >= entry));
    match (by_row, by_entry) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn max_entry(row: &Row) -> usize {
    match row {
        Row::Entry(index) => *index,
        Row::Work { entries, .. } => entries.iter().copied().max().unwrap_or(0),
        Row::Fold { rows, .. } => rows.iter().map(max_entry).max().unwrap_or(0),
    }
}

struct RowContext<'a> {
    chat: &'a Chat,
    expanded: &'a HashSet<String>,
    workspace: &'a WeakEntity<Workspace>,
    /// The list index of the top-level row, to re-measure it when it expands.
    index: usize,
}

fn toggle(
    context: &RowContext<'_>,
    key: String,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let workspace = context.workspace.clone();
    let index = context.index;
    move |_, _, cx| {
        let key = key.clone();
        let _ = workspace.update(cx, |this, cx| this.toggle_expanded(key, index, cx));
    }
}

fn work_glyph(icon_kind: WorkIcon) -> Icon {
    match icon_kind {
        WorkIcon::Terminal => Icon::Terminal,
        WorkIcon::Edit => Icon::Pencil,
        WorkIcon::Thought => Icon::Lightbulb,
        WorkIcon::Search => Icon::Search,
        WorkIcon::Tool => Icon::Hammer,
    }
}

/// "Running mkdir" for a live group, from its newest running command.
fn running_label(entries: &[usize], chat: &Chat) -> Option<String> {
    entries
        .iter()
        .rev()
        .find_map(|&index| match &chat.entries[index].kind {
            EntryKind::Command {
                command,
                running: true,
                ..
            } => {
                let program = command
                    .split_whitespace()
                    .find(|word| !word.contains('='))
                    .map(|word| word.rsplit('/').next().unwrap_or(word))
                    .unwrap_or("command");
                Some(format!("Running {program}"))
            }
            EntryKind::Tool {
                name,
                running: true,
                ..
            } => Some(format!(
                "Running {}",
                name.rsplit("__").next().unwrap_or(name)
            )),
            EntryKind::Reasoning {
                streaming: true, ..
            } => Some("Thinking".to_owned()),
            _ => None,
        })
}

/// A muted one-line row with an icon, like "Ran 3 commands" or "Worked for 2m".
fn summary_line(id: String, glyph: AnyElement, label: String) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .flex()
        .items_center()
        .gap_2p5()
        .h(px(24.))
        .text_size(px(14.))
        .text_color(theme::text_muted())
        .cursor_pointer()
        .hover(|style| style.text_color(theme::text()))
        .child(
            div()
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .child(glyph),
        )
        .child(one_line(label))
}

fn render_row(row: &Row, context: &RowContext<'_>) -> AnyElement {
    match row {
        Row::Entry(index) => render_entry(*index, context),
        Row::Work {
            key,
            entries,
            summary,
            icon: icon_kind,
            running,
        } => {
            let expanded = context.expanded.contains(key);
            let (glyph, label) = if *running {
                (
                    spinner(format!("spin-{key}"), 14., theme::text_muted()).into_any_element(),
                    running_label(entries, context.chat).unwrap_or_else(|| summary.clone()),
                )
            } else {
                (
                    icon(work_glyph(*icon_kind), 14., theme::text_muted()).into_any_element(),
                    summary.clone(),
                )
            };
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    summary_line(key.clone(), glyph, label).on_click(toggle(context, key.clone())),
                )
                .when(expanded, |group| {
                    group.child(
                        div()
                            .ml(px(7.))
                            .pl_4()
                            .border_l_1()
                            .border_color(theme::border())
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(
                                entries
                                    .iter()
                                    .map(|&index| render_work_detail(index, context)),
                            ),
                    )
                })
                .into_any_element()
        }
        Row::Fold { key, label, rows } => {
            let expanded = context.expanded.contains(key);
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    summary_line(
                        key.clone(),
                        icon(Icon::CircleCheck, 14., theme::text_muted()).into_any_element(),
                        label.clone(),
                    )
                    .on_click(toggle(context, key.clone())),
                )
                .when(expanded, |fold| {
                    fold.child(
                        div()
                            .ml(px(7.))
                            .pl_4()
                            .border_l_1()
                            .border_color(theme::border())
                            .flex()
                            .flex_col()
                            .gap_3()
                            .children(rows.iter().map(|row| render_row(row, context))),
                    )
                })
                .into_any_element()
        }
    }
}

/// Long command output is cut to its last lines, as the web transcript does.
fn tail(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.trim_end().to_owned();
    }
    format!(
        "… {} earlier lines\n{}",
        lines.len() - max_lines,
        lines[lines.len() - max_lines..].join("\n")
    )
}

fn mono_block(text: String, id: String) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .w_full()
        .overflow_x_scroll()
        .px_3()
        .py_2()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::card())
        .font_family(theme::mono())
        .text_size(px(12.))
        .line_height(px(18.))
        .text_color(theme::prose())
        .whitespace_nowrap()
        .child(text)
}

/// One step inside an expanded work group.
fn render_work_detail(index: usize, context: &RowContext<'_>) -> AnyElement {
    let entry = &context.chat.entries[index];
    let line = |glyph: Icon, text: String, color: Hsla| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(13.))
            .text_color(color)
            .child(icon(glyph, 13., theme::text_faint()))
            .child(one_line(text))
    };
    match &entry.kind {
        EntryKind::Command {
            command,
            output,
            exit_code,
            failed,
            ..
        } => div()
            .flex()
            .flex_col()
            .gap_1()
            .child(mono_block(
                format!("$ {command}"),
                format!("cmd-{}", entry.key),
            ))
            .children(
                output
                    .as_deref()
                    .filter(|output| !output.trim().is_empty())
                    .map(|output| mono_block(tail(output, 12), format!("out-{}", entry.key))),
            )
            .when(*failed, |command| {
                command.child(div().text_size(px(12.)).text_color(theme::danger()).child(
                    match exit_code {
                        Some(code) => format!("Exited with code {code}"),
                        None => "Failed".to_owned(),
                    },
                ))
            })
            .into_any_element(),
        EntryKind::FileChange {
            path,
            additions,
            deletions,
        } => div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(13.))
            .child(icon(Icon::Pencil, 13., theme::text_faint()))
            .child(
                one_line(path.clone())
                    .font_family(theme::mono())
                    .text_size(px(12.))
                    .text_color(theme::prose()),
            )
            .children(additions.map(|count| {
                div()
                    .text_color(theme::success())
                    .child(format!("+{count}"))
            }))
            .children(
                deletions.map(|count| div().text_color(theme::danger()).child(format!("−{count}"))),
            )
            .into_any_element(),
        EntryKind::Reasoning { text, .. } => div()
            .text_size(px(13.))
            .line_height(px(20.))
            .text_color(theme::text_muted())
            .child(t3_state::title::truncate(text, 1200))
            .into_any_element(),
        EntryKind::Search { label } => line(
            Icon::Search,
            format!("Searched {label}"),
            theme::text_muted(),
        )
        .into_any_element(),
        EntryKind::Tool { name, failed, .. } => line(
            Icon::Hammer,
            name.clone(),
            if *failed {
                theme::danger()
            } else {
                theme::text_muted()
            },
        )
        .into_any_element(),
        EntryKind::Other { label } => {
            line(Icon::Hammer, label.clone(), theme::text_muted()).into_any_element()
        }
        _ => render_entry(index, context),
    }
}

fn format_local_time(iso: &str) -> Option<String> {
    let time = chrono::DateTime::parse_from_rfc3339(iso).ok()?;
    Some(
        time.with_timezone(&chrono::Local)
            .format("%-I:%M %p")
            .to_string(),
    )
}

fn card(border: Hsla) -> gpui::Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_xl()
        .border_1()
        .border_color(border)
        .bg(theme::card())
}

fn card_title(glyph: Icon, text: &'static str, color: Hsla) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(icon(glyph, 14., color))
        .child(text)
}

fn render_entry(index: usize, context: &RowContext<'_>) -> AnyElement {
    let entry = &context.chat.entries[index];
    let parsed = context.chat.markdown.get(index).cloned().flatten();
    match &entry.kind {
        EntryKind::User { text } => div()
            .w_full()
            .flex()
            .flex_col()
            .items_end()
            .gap_1()
            .child(
                div()
                    .max_w(gpui::relative(0.8))
                    .px_4()
                    .py_2p5()
                    .rounded(px(18.))
                    .bg(theme::message())
                    .text_size(px(14.))
                    .line_height(px(markdown::LINE_HEIGHT))
                    .text_color(theme::text())
                    .child(text.clone()),
            )
            .children(
                entry
                    .meta
                    .started_at
                    .as_deref()
                    .and_then(format_local_time)
                    .map(|time| {
                        div()
                            .px_1()
                            .text_size(px(12.))
                            .text_color(theme::text_muted())
                            .child(time)
                    }),
            )
            .into_any_element(),
        EntryKind::Assistant { text, streaming } => {
            if text.trim().is_empty() && *streaming {
                return div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(14.))
                    .text_color(theme::text_muted())
                    .child(spinner(
                        format!("spin-{}", entry.key),
                        14.,
                        theme::text_muted(),
                    ))
                    .child("Thinking")
                    .into_any_element();
            }
            match parsed {
                Some(blocks) => markdown::render(&blocks, &entry.key).into_any_element(),
                None => div().child(text.clone()).into_any_element(),
            }
        }
        EntryKind::Plan { markdown: text, .. } => card(theme::border())
            .child(card_title(Icon::ListTodo, "Plan", theme::text_muted()))
            .child(match parsed {
                Some(blocks) => markdown::render(&blocks, &entry.key).into_any_element(),
                None => div().child(text.clone()).into_any_element(),
            })
            .into_any_element(),
        EntryKind::Todo { steps } => card(theme::border())
            .child(card_title(Icon::ListTodo, "To-dos", theme::text_muted()))
            .children(steps.iter().map(|step| {
                let (glyph, color, text_color) = match step.status {
                    TodoStatus::Completed => {
                        (Icon::CircleCheck, theme::success(), theme::text_muted())
                    }
                    TodoStatus::Running => (Icon::LoaderCircle, theme::info(), theme::text()),
                    TodoStatus::Pending => (Icon::Circle, theme::text_faint(), theme::prose()),
                };
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(14.))
                    .text_color(text_color)
                    .child(icon(glyph, 14., color))
                    .child(step.text.clone())
            }))
            .into_any_element(),
        EntryKind::Approval {
            request_id,
            kind,
            prompt,
            pending,
        } => {
            let mut approval = card(if *pending {
                theme::warning().opacity(0.4)
            } else {
                theme::border()
            })
            .child(card_title(
                Icon::ShieldAlert,
                if *pending {
                    "Approval needed"
                } else {
                    "Approval"
                },
                theme::warning(),
            ))
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme::text_muted())
                    .child(kind.clone()),
            )
            .children(
                prompt
                    .clone()
                    .map(|prompt| mono_block(prompt, format!("prompt-{}", entry.key))),
            );
            if *pending {
                let choices = [
                    ("Allow", ApprovalDecision::Accept, true),
                    (
                        "Allow for session",
                        ApprovalDecision::AcceptForSession,
                        false,
                    ),
                    ("Deny", ApprovalDecision::Decline, false),
                ];
                approval = approval.child(div().flex().gap_2().pt_1().children(
                    choices.into_iter().map(|(label, decision, primary)| {
                        let workspace = context.workspace.clone();
                        let request_id = request_id.clone();
                        button(format!("approval-{request_id}-{label}"), label, primary).on_click(
                            move |_: &ClickEvent, _, cx| {
                                let request_id = request_id.clone();
                                let _ = workspace
                                    .update(cx, |this, cx| this.respond(request_id, decision, cx));
                            },
                        )
                    }),
                ));
            }
            approval.into_any_element()
        }
        EntryKind::Question {
            prompt, pending, ..
        } => card(if *pending {
            theme::info().opacity(0.4)
        } else {
            theme::border()
        })
        .child(card_title(Icon::CircleHelp, "Question", theme::info()))
        .child(
            div()
                .text_size(px(14.))
                .line_height(px(markdown::LINE_HEIGHT))
                .text_color(theme::prose())
                .child(prompt.clone()),
        )
        .when(*pending, |question| {
            question.child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::text_muted())
                    .child("Reply in the composer to answer."),
            )
        })
        .into_any_element(),
        EntryKind::Error { message } => div()
            .flex()
            .items_start()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(theme::danger().opacity(0.3))
            .bg(theme::danger().opacity(0.06))
            .text_size(px(13.))
            .line_height(px(20.))
            .text_color(theme::danger())
            .child(
                div()
                    .pt(px(3.))
                    .child(icon(Icon::CircleAlert, 14., theme::danger())),
            )
            .child(div().flex_1().min_w_0().child(message.clone()))
            .into_any_element(),
        EntryKind::Notice { text } => div()
            .w_full()
            .flex()
            .justify_center()
            .text_size(px(12.))
            .text_color(theme::text_muted())
            .child(text.clone())
            .into_any_element(),
        // Work entries normally sit in a summary row; drawn alone they use their detail view.
        _ => render_work_detail(index, context),
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &NewThread, window, cx| {
                let project = this.selected_project.clone();
                this.start_new_thread(project, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ConnectRemote, window, cx| {
                this.open_dialog(Dialog::Pair, window, cx)
            }))
            .on_action(cx.listener(|this, _: &AddProject, window, cx| {
                this.open_dialog(Dialog::AddProject, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Search, window, cx| this.start_search(window, cx)))
            .on_action(
                cx.listener(|this, _: &CloseDialog, window, cx| this.close_dialog(window, cx)),
            )
            .on_action(cx.listener(|_, _: &Quit, _, cx| cx.quit()))
            .size_full()
            .relative()
            .flex()
            .bg(theme::background())
            .font_family(theme::SANS)
            .text_color(theme::text())
            .when(self.sidebar_open, |workspace| {
                workspace.child(self.render_sidebar(cx))
            })
            .child(self.render_main(cx))
            .children(self.render_dialog(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use t3_state::transcript::EntryMeta;

    #[test]
    fn tails_long_output() {
        assert_eq!(tail("a\nb\n", 3), "a\nb");
        assert_eq!(tail("1\n2\n3\n4\n5", 2), "… 3 earlier lines\n4\n5");
        assert_eq!(tail("", 2), "");
    }

    fn entry(key: &str, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            key: key.into(),
            kind: EntryKind::Assistant {
                text: text.into(),
                streaming: false,
            },
            meta: EntryMeta::default(),
        }
    }

    #[test]
    fn rows_remeasure_from_the_first_change() {
        let entries = vec![entry("a", "1"), entry("b", "2")];
        let rows = vec![Row::Entry(0), Row::Entry(1)];
        let old = Chat {
            entries: entries.clone(),
            rows: rows.clone(),
            markdown: Vec::new(),
        };
        assert_eq!(first_changed_row(&old, &rows, &entries), None);
        let streamed = vec![entry("a", "1"), entry("b", "2 more")];
        assert_eq!(first_changed_row(&old, &rows, &streamed), Some(1));
        let grown = vec![entry("a", "1"), entry("b", "2"), entry("c", "3")];
        let grown_rows = vec![Row::Entry(0), Row::Entry(1), Row::Entry(2)];
        assert_eq!(first_changed_row(&old, &grown_rows, &grown), Some(2));
        let folded = vec![
            Row::Fold {
                key: "fold:r".into(),
                label: "Worked for 1s".into(),
                rows: vec![Row::Entry(0)],
            },
            Row::Entry(1),
        ];
        assert_eq!(first_changed_row(&old, &folded, &entries), Some(0));
    }

    #[test]
    fn running_rows_name_the_program() {
        let command = |command: &str| TranscriptEntry {
            key: "c".into(),
            kind: EntryKind::Command {
                command: command.into(),
                output: None,
                exit_code: None,
                running: true,
                failed: false,
            },
            meta: EntryMeta::default(),
        };
        let chat = Chat {
            entries: vec![
                command("RUST_LOG=debug /usr/bin/cargo test"),
                command("mkdir -p x"),
            ],
            rows: Vec::new(),
            markdown: Vec::new(),
        };
        assert_eq!(running_label(&[0], &chat).as_deref(), Some("Running cargo"));
        assert_eq!(
            running_label(&[0, 1], &chat).as_deref(),
            Some("Running mkdir")
        );
    }
}
