//! The lazygit-style approval TUI.
//!
//! Two sections: the runs waiting on a deployment review, and the recent
//! runs. Keys: j/k move, h/l (or 1/2/Tab) switches sections, Enter opens the
//! environment review of a waiting run (space toggles the marked set, Enter
//! submits an approval, x submits a rejection), a/R approve or reject every
//! reviewable environment of the run, o opens the run in the browser. The
//! recent section can also cancel a run (c), rerun it (r), or rerun its
//! failed jobs (f). g refreshes, ? shows the key map, q quits.
//!
//! The data fetches on a worker thread, so the UI never blocks on `gh`.

use std::collections::HashMap;
use std::error::Error;
use std::io::{self, IsTerminal};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event as Ev, KeyCode, KeyEvent, KeyEventKind};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::execute;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::config::Config;
use crate::github::{Client, PendingEnv, Run};
use crate::text::{rel_age, truncate};

const REVIEW_COMMENT_APPROVE: &str = "approved with iris";
const REVIEW_COMMENT_REJECT: &str = "rejected with iris";
const RECENT_COUNT: usize = 12;

/// Everything the fetch worker sends to the UI.
#[derive(Debug, Clone, Default)]
struct Model {
    waiting: Vec<Run>,
    envs: HashMap<u64, Vec<PendingEnv>>,
    recent: Vec<Run>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Waiting,
    Recent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Approve,
    Reject,
}

/// The plan a confirmation popup will run.
enum Plan {
    Review {
        run_id: u64,
        env_ids: Vec<u64>,
        action: Action,
    },
    Cancel {
        run_id: u64,
    },
    Rerun {
        run_id: u64,
        failed_only: bool,
    },
}

enum Modal {
    Review {
        run_id: u64,
        envs: Vec<PendingEnv>,
        marked: Vec<bool>,
        sel: usize,
    },
    Confirm {
        text: Vec<String>,
        plan: Plan,
    },
    Help,
}

struct App {
    cfg: Config,
    client: Client,
    model: Option<Model>,
    rx: mpsc::Receiver<Model>,
    req_tx: mpsc::Sender<()>,
    section: Section,
    sel_waiting: usize,
    sel_recent: usize,
    offset_waiting: usize,
    offset_recent: usize,
    modal: Option<Modal>,
    status: String,
    busy: bool,
}

pub fn run(cfg: &Config) {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        eprintln!(
            "iris: the TUI needs a terminal. Run `iris` inside tmux or a terminal emulator."
        );
        std::process::exit(2);
    }
    // Broken raw-mode output on panic looks like garbage; leave the screen
    // first so the message stays readable.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave_terminal();
        hook(info);
    }));

    if enable_raw_mode().is_err() {
        return;
    }
    if execute!(io::stdout(), EnterAlternateScreen).is_err() {
        let _ = disable_raw_mode();
        return;
    }
    let ui_result = drive(cfg);
    leave_terminal();
    if let Err(e) = ui_result {
        eprintln!("iris: {e}");
        std::process::exit(1);
    }
}

/// The panic hook already returns to the main screen, so the hook runs the
/// crash and exit path; here only the regular error path restores.
fn drive(cfg: &Config) -> Result<(), Box<dyn Error>> {
    let client = Client::new(&cfg.repo);
    let (rx, req_tx) = spawn_worker(client.clone(), cfg.clone());

    let mut app = App {
        cfg: cfg.clone(),
        client,
        model: None,
        rx,
        req_tx,
        section: Section::Waiting,
        sel_waiting: 0,
        sel_recent: 0,
        offset_waiting: 0,
        offset_recent: 0,
        modal: None,
        status: "loading…".to_string(),
        busy: false,
    };

    let mut term: Terminal<CrosstermBackend<io::Stdout>> =
        Terminal::new(CrosstermBackend::new(io::stdout()))?;
    loop {
        term.draw(|f| draw(f, &mut app))?;
        if event::poll(Duration::from_millis(120))? {
            if let Ev::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && app.on_key(key) {
                    return Ok(());
                }
            }
        }
        let mut newest = None;
        while let Ok(model) = app.rx.try_recv() {
            newest = Some(model);
        }
        if let Some(model) = newest {
            app.apply(model);
        }
    }
}

fn leave_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

fn spawn_worker(client: Client, cfg: Config) -> (mpsc::Receiver<Model>, mpsc::Sender<()>) {
    let (model_tx, model_rx) = mpsc::channel::<Model>();
    let (req_tx, req_rx) = mpsc::channel::<()>();
    let _ = std::thread::Builder::new()
        .name("iris-fetch".to_string())
        .spawn(move || loop {
            let model = fetch(&client);
            if model_tx.send(model).is_err() {
                return;
            }
            let deadline = Instant::now() + Duration::from_secs(cfg.auto_secs);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                match req_rx.recv_timeout(left) {
                    Ok(()) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        });
    (model_rx, req_tx)
}

fn fetch(client: &Client) -> Model {
    let waiting = client.waiting().unwrap_or_default();
    let mut envs: HashMap<u64, Vec<PendingEnv>> = HashMap::new();
    for run in &waiting {
        if let Ok(list) = client.pending(run.id) {
            envs.insert(run.id, list);
        }
    }
    let recent = client.recent(RECENT_COUNT).unwrap_or_default();
    Model {
        waiting,
        envs,
        recent,
    }
}

impl App {
    fn apply(&mut self, model: Model) {
        self.model = Some(model);
        let model = self.model.as_ref().expect("just set");
        self.sel_waiting = self
            .sel_waiting
            .min(model.waiting.len().saturating_sub(1));
        self.sel_recent = self.sel_recent.min(model.recent.len().saturating_sub(1));
        self.status = "refreshed".to_string();
    }

    fn runs_len(&self, section: Section) -> usize {
        let Some(model) = self.model.as_ref() else {
            return 0;
        };
        match section {
            Section::Waiting => model.waiting.len(),
            Section::Recent => model.recent.len(),
        }
    }

    fn selected_run(&self) -> Option<&Run> {
        let model = self.model.as_ref()?;
        let list = match self.section {
            Section::Waiting => &model.waiting,
            Section::Recent => &model.recent,
        };
        let index = match self.section {
            Section::Waiting => self.sel_waiting,
            Section::Recent => self.sel_recent,
        };
        list.get(index)
    }

    fn pending_envs(&self, run_id: u64) -> Vec<PendingEnv> {
        self.model
            .as_ref()
            .and_then(|m| m.envs.get(&run_id).cloned())
            .unwrap_or_default()
    }

    fn reviewable_envs(&self, run_id: u64) -> Vec<PendingEnv> {
        self.pending_envs(run_id)
            .into_iter()
            .filter(|e| e.can_approve)
            .collect()
    }

    fn refresh(&mut self) {
        let _ = self.req_tx.send(());
        self.status = "refreshing…".to_string();
    }

    fn open(&mut self, run_id: u64) {
        let url = format!("https://github.com/{}/actions/runs/{run_id}", self.cfg.repo);
        let _ = Command::new("xdg-open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        self.status = "opened the run in the browser".to_string();
    }

    fn ask_review_all(&mut self, action: Action) {
        let Some(run) = self.selected_run().cloned() else {
            return;
        };
        let envs = self.reviewable_envs(run.id);
        if envs.is_empty() {
            self.status = "nothing to review: not waiting, or not approvable".to_string();
            return;
        }
        let names = envs
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let verb = match action {
            Action::Approve => "Approve",
            Action::Reject => "Reject",
        };
        let text = vec![
            format!("{verb} every reviewable environment of run #{}?", run.id),
            format!("{} · {}", run.name(), truncate(run.title(), self.cfg.title_max)),
            format!("environments: {names}"),
            String::new(),
            "[y] yes   [n/Esc] no".to_string(),
        ];
        self.modal = Some(Modal::Confirm {
            text,
            plan: Plan::Review {
                run_id: run.id,
                env_ids: envs.iter().map(|e| e.id).collect(),
                action,
            },
        });
    }

    fn open_review(&mut self) {
        let Some(run) = self.selected_run().cloned() else {
            return;
        };
        let envs = self.pending_envs(run.id);
        if envs.is_empty() {
            self.status = "no pending environments for this run".to_string();
            return;
        }
        let marked = envs.iter().map(|e| e.can_approve).collect();
        self.modal = Some(Modal::Review {
            run_id: run.id,
            envs,
            marked,
            sel: 0,
        });
    }

    fn ask_cancel(&mut self) {
        let Some(run) = self.selected_run().cloned() else {
            return;
        };
        let text = vec![
            format!("Cancel run #{}?", run.id),
            format!("{} · {}", run.name(), truncate(run.title(), self.cfg.title_max)),
            String::new(),
            "[y] yes   [n/Esc] no".to_string(),
        ];
        self.modal = Some(Modal::Confirm {
            text,
            plan: Plan::Cancel { run_id: run.id },
        });
    }

    fn ask_rerun(&mut self, failed_only: bool) {
        let Some(run) = self.selected_run().cloned() else {
            return;
        };
        let what = if failed_only { "the failed jobs of " } else { "" };
        let text = vec![
            format!("Re-run {what}run #{}?", run.id),
            format!("{} · {}", run.name(), truncate(run.title(), self.cfg.title_max)),
            String::new(),
            "[y] yes   [n/Esc] no".to_string(),
        ];
        self.modal = Some(Modal::Confirm {
            text,
            plan: Plan::Rerun {
                run_id: run.id,
                failed_only,
            },
        });
    }

    fn execute(&mut self, plan: Plan) {
        self.busy = true;
        self.modal = None;
        self.status = "working…".to_string();
        let outcome: Result<(), String> = match &plan {
            Plan::Review {
                run_id,
                env_ids,
                action,
            } => {
                let (state, comment) = match action {
                    Action::Approve => ("approved", REVIEW_COMMENT_APPROVE),
                    Action::Reject => ("rejected", REVIEW_COMMENT_REJECT),
                };
                self.client
                    .review(*run_id, env_ids, state, comment)
                    .map_err(|e| e.to_string())
            }
            Plan::Cancel { run_id } => {
                self.client.action(*run_id, "cancel").map_err(|e| e.to_string())
            }
            Plan::Rerun {
                run_id,
                failed_only,
            } => {
                let act = if *failed_only {
                    "rerun-failed-jobs"
                } else {
                    "rerun"
                };
                self.client.action(*run_id, act).map_err(|e| e.to_string())
            }
        };
        self.busy = false;
        let run_id = match &plan {
            Plan::Review { run_id, .. }
            | Plan::Cancel { run_id }
            | Plan::Rerun { run_id, .. } => *run_id,
        };
        match outcome {
            Ok(()) => {
                self.status = format!("✔ done: run #{run_id}");
                self.refresh();
            }
            Err(e) => self.status = format!("✘ {e}"),
        }
    }

    /// Returns true when the app should quit.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if self.busy {
            return false;
        }
        match self.modal.take() {
            None => self.on_key_plain(key),
            Some(Modal::Help) => false, // closes on any key
            Some(Modal::Confirm { text, plan }) => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => self.execute(plan),
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N')
                    | KeyCode::Char('q') => self.modal = None,
                    _ => {
                        self.modal = Some(Modal::Confirm { text, plan });
                    }
                }
                false
            }
            Some(Modal::Review {
                run_id,
                envs,
                marked,
                sel,
            }) => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        let sel = sel.saturating_sub(1);
                        self.modal = Some(Modal::Review { run_id, envs, marked, sel });
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let sel = (sel + 1).min(envs.len().saturating_sub(1));
                        self.modal = Some(Modal::Review { run_id, envs, marked, sel });
                    }
                    KeyCode::Char(' ') => {
                        let mut marked = marked;
                        if let Some(m) = marked.get_mut(sel) {
                            *m = !*m;
                        }
                        self.modal = Some(Modal::Review { run_id, envs, marked, sel });
                    }
                    KeyCode::Enter => {
                        let env_ids: Vec<u64> = envs
                            .iter()
                            .zip(&marked)
                            .filter(|(_, m)| **m)
                            .map(|(e, _)| e.id)
                            .collect();
                        if env_ids.is_empty() {
                            self.status = "no environments marked".to_string();
                            self.modal =
                                Some(Modal::Review { run_id, envs, marked, sel });
                        } else {
                            self.execute(Plan::Review {
                                run_id,
                                env_ids,
                                action: Action::Approve,
                            });
                        }
                    }
                    KeyCode::Char('x') => {
                        let env_ids: Vec<u64> = envs
                            .iter()
                            .zip(&marked)
                            .filter(|(_, m)| **m)
                            .map(|(e, _)| e.id)
                            .collect();
                        if env_ids.is_empty() {
                            self.status = "no environments marked".to_string();
                            self.modal =
                                Some(Modal::Review { run_id, envs, marked, sel });
                        } else {
                            self.execute(Plan::Review {
                                run_id,
                                env_ids,
                                action: Action::Reject,
                            });
                        }
                    }
                    _ => {
                        self.modal = Some(Modal::Review { run_id, envs, marked, sel });
                    }
                }
                false
            }
        }
    }

    fn on_key_plain(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return true,
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('g') => self.refresh(),
            KeyCode::Tab
            | KeyCode::Char('1')
            | KeyCode::Char('2')
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                self.section = match self.section {
                    Section::Waiting => Section::Recent,
                    Section::Recent => Section::Waiting,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            _ => {}
        }
        // The section-specific actions.
        let Some(run) = self.selected_run().cloned() else {
            return false;
        };
        match self.section {
            Section::Waiting => match key.code {
                KeyCode::Char('a') => self.ask_review_all(Action::Approve),
                KeyCode::Char('R') => self.ask_review_all(Action::Reject),
                KeyCode::Enter => self.open_review(),
                KeyCode::Char('o') => self.open(run.id),
                _ => {}
            },
            Section::Recent => match key.code {
                KeyCode::Char('c') => self.ask_cancel(),
                KeyCode::Char('r') => self.ask_rerun(false),
                KeyCode::Char('f') => self.ask_rerun(true),
                KeyCode::Char('o') | KeyCode::Enter => self.open(run.id),
                _ => {}
            },
        }
        false
    }

    fn move_sel(&mut self, delta: i64) {
        let len = self.runs_len(self.section);
        if len == 0 {
            return;
        }
        let index = match self.section {
            Section::Waiting => &mut self.sel_waiting,
            Section::Recent => &mut self.sel_recent,
        };
        *index = ((*index as i64) + delta).clamp(0, len as i64 - 1) as usize;
    }
}

fn section_label(app: &App, section: Section) -> String {
    let n = app.runs_len(section);
    let key = if section == Section::Waiting { 1 } else { 2 };
    let name = if section == Section::Waiting {
        "waiting"
    } else {
        "recent"
    };
    format!("[{key}] {name} ({n})")
}

fn icon_for(run: &Run) -> (char, Color) {
    match run.status.as_deref().unwrap_or("") {
        "waiting" => ('⚑', Color::Indexed(214)),
        "queued" => ('◌', Color::Indexed(39)),
        "in_progress" => ('▶', Color::Indexed(39)),
        "completed" => match run.conclusion.as_deref() {
            Some("success") => ('✔', Color::Indexed(114)),
            Some("cancelled") => ('⊘', Color::Indexed(245)),
            Some(_) => ('✘', Color::Indexed(203)),
            None => ('·', Color::Indexed(245)),
        },
        _ => ('·', Color::Indexed(245)),
    }
}

fn run_line(app: &App, run: &Run, focused: bool) -> Line<'static> {
    let (icon, color) = icon_for(run);
    let mut spans = Vec::new();
    if focused {
        spans.push(Span::styled(
            "▌ ",
            Style::new().fg(Color::Indexed(214)).add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(
        format!("{icon} "),
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!("{:<13}", run.name()),
        Style::new().fg(Color::Indexed(253)),
    ));
    spans.push(Span::styled(
        format!("{:>5} ", rel_age(run.created_at.as_deref())),
        Style::new().fg(Color::Indexed(244)),
    ));
    spans.push(Span::styled(
        truncate(run.title(), app.cfg.title_max.min(60)),
        Style::new().fg(Color::Indexed(252)),
    ));
    if let Some(branch) = run.head_branch.clone() {
        spans.push(Span::styled(
            format!(" {branch}"),
            Style::new().fg(Color::Indexed(244)),
        ));
    }
    Line::from(spans)
}

fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(7),
        Constraint::Length(1),
    ])
    .split(f.area());

    draw_header(f, app, chunks[0]);
    draw_list(f, app, chunks[1]);
    draw_detail(f, app, chunks[2]);
    draw_footer(f, app, chunks[3]);

    match &app.modal {
        Some(Modal::Review { .. }) => draw_review_modal(f, app),
        Some(Modal::Confirm { .. }) => draw_confirm_modal(f, app),
        Some(Modal::Help) => draw_help_modal(f),
        None => {}
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled("iris", Style::new().bold().fg(Color::Indexed(255))),
        Span::styled(
            format!(" · {}", app.cfg.repo),
            Style::new().fg(Color::Indexed(245)),
        ),
        Span::raw("   "),
    ];
    for section in [Section::Waiting, Section::Recent] {
        let focused = app.section == section;
        let style = if focused {
            Style::new().bold().fg(Color::Indexed(214))
        } else {
            Style::new().fg(Color::Indexed(244))
        };
        spans.push(Span::styled(section_label(app, section), style));
        spans.push(Span::raw("  "));
    }
    if app.busy {
        spans.push(Span::styled(
            " · working…",
            Style::new().fg(Color::Indexed(214)),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let list: Vec<Run> = match app.model.as_ref() {
        None => {
            f.render_widget(
                Paragraph::new("loading…").block(Block::bordered().title(" runs ")),
                area,
            );
            return;
        }
        Some(model) => match app.section {
            Section::Waiting => model.waiting.clone(),
            Section::Recent => model.recent.clone(),
        },
    };
    let title = match app.section {
        Section::Waiting => " deployments waiting for your approval ",
        Section::Recent => " the most recent runs ",
    };
    let mut lines: Vec<Line> = Vec::new();
    if list.is_empty() {
        let empty = match app.section {
            Section::Waiting => "nothing waits for approval — clean house",
            Section::Recent => "no runs yet",
        };
        lines.push(Line::styled(
            empty.to_string(),
            Style::new().fg(Color::Indexed(244)),
        ));
    }
    let sel = match app.section {
        Section::Waiting => app.sel_waiting,
        Section::Recent => app.sel_recent,
    };
    for (i, run) in list.iter().enumerate() {
        lines.push(run_line(app, run, i == sel));
    }

    let mut offset = match app.section {
        Section::Waiting => app.offset_waiting,
        Section::Recent => app.offset_recent,
    };
    let visible = area.height.saturating_sub(2) as usize;
    if sel < offset {
        offset = sel;
    }
    if visible > 0 && sel >= offset + visible {
        offset = sel + 1 - visible;
    }
    match app.section {
        Section::Waiting => app.offset_waiting = offset,
        Section::Recent => app.offset_recent = offset,
    }

    let shown: Vec<Line> = lines.into_iter().skip(offset).take(visible).collect();
    f.render_widget(Paragraph::new(shown).block(Block::bordered().title(title)), area);
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let Some(run) = app.selected_run().cloned() else {
        f.render_widget(
            Paragraph::new("select a run above".to_string())
                .block(Block::bordered().title(" run detail ")),
            area,
        );
        return;
    };
    let title = format!(" run #{} ", run.id);
    let age = rel_age(run.created_at.as_deref());
    let status = run.status.as_deref().unwrap_or("-");
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            truncate(run.title(), app.cfg.title_max),
            Style::new().bold().fg(Color::Indexed(255)),
        ),
    ]));
    lines.push(Line::styled(
        format!(
            "{:<13} status: {:<12} conclusion: {:<10} branch: {:<10} started {} ago",
            run.name(),
            status,
            run.conclusion.as_deref().unwrap_or("-"),
            run.head_branch.clone().unwrap_or_else(|| "-".into()),
            age
        ),
        Style::new().fg(Color::Indexed(244)),
    ));
    if app.section == Section::Waiting {
        let envs = app.pending_envs(run.id);
        if envs.is_empty() {
            lines.push(Line::styled(
                "waiting for approval (environments unknown)".to_string(),
                Style::new().fg(Color::Indexed(214)),
            ));
        } else {
            let mut spans = vec![Span::styled(
                "environments: ",
                Style::new().fg(Color::Indexed(214)),
            )];
            for (i, env) in envs.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(", "));
                }
                let mut text = env.name.clone();
                if !env.can_approve {
                    text.push_str(" (cannot approve)");
                }
                let style = if env.can_approve {
                    Style::new().bold().fg(Color::Indexed(214))
                } else {
                    Style::new().fg(Color::Indexed(244)).add_modifier(Modifier::DIM)
                };
                spans.push(Span::styled(text, style));
            }
            lines.push(Line::from(spans));
        }
    } else {
        lines.push(Line::styled(
            format!("concluded: {}", run.conclusion.as_deref().unwrap_or("-")),
            Style::new().fg(Color::Indexed(244)),
        ));
    }
    lines.push(Line::styled(
        format!(
            "https://github.com/{}/actions/runs/{}",
            app.cfg.repo, run.id
        ),
        Style::new().fg(Color::Indexed(243)),
    ));
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        area,
    );
}

fn footer_help(app: &App) -> String {
    match app.section {
        Section::Waiting => {
            "j/k move · h/l section · a approve-all · R reject-all · Enter review envs · o open · g refresh · ? help · q quit".to_string()
        }
        Section::Recent => {
            "j/k move · h/l section · c cancel · r rerun · f rerun-failed · o open · g refresh · ? help · q quit".to_string()
        }
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let parts = Layout::horizontal([Constraint::Percentage(72), Constraint::Percentage(28)])
        .split(area);
    f.render_widget(
        Paragraph::new(footer_help(app)).style(Style::new().fg(Color::Indexed(244))),
        parts[0],
    );
    f.render_widget(
        Paragraph::new(app.status.clone())
            .alignment(Alignment::Right)
            .style(Style::new().fg(Color::Indexed(214))),
        parts[1],
    );
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2)).max(4);
    let h = h.min(area.height.saturating_sub(2)).max(4);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn inner(rect: Rect) -> Rect {
    Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    }
}

fn draw_review_modal(f: &mut Frame, app: &App) {
    let Modal::Review {
        run_id,
        envs,
        marked,
        sel,
    } = app.modal.as_ref().expect("review modal")
    else {
        return;
    };
    let width = 62u16.max(4);
    let height = (envs.len() as u16).saturating_add(5).max(6);
    let rect = centered(f.area(), width, height);
    f.render_widget(
        Block::bordered()
            .title(format!(" Review run #{run_id} "))
            .style(Style::new().fg(Color::Indexed(214))),
        rect,
    );
    let area = inner(rect);
    let mut lines = Vec::new();
    for (i, env) in envs.iter().enumerate() {
        let mark = if *marked.get(i).unwrap_or(&false) {
            "[x] "
        } else {
            "[ ] "
        };
        let cursor = if i == *sel { "› " } else { "  " };
        let mut spans = vec![
            Span::styled(cursor, Style::new().fg(Color::Indexed(214))),
            Span::styled(mark, Style::new().fg(Color::Indexed(214))),
        ];
        if env.can_approve {
            spans.push(Span::styled(
                env.name.clone(),
                Style::new().bold().fg(Color::Indexed(253)),
            ));
            spans.push(Span::styled(
                format!("  (id {})", env.id),
                Style::new().fg(Color::Indexed(244)).add_modifier(Modifier::DIM),
            ));
        } else {
            spans.push(Span::styled(
                format!("{} (cannot approve — others must review this env)", env.name),
                Style::new().fg(Color::Indexed(244)).add_modifier(Modifier::DIM),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::raw(String::new()));
    lines.push(Line::styled(
        "space toggle · Enter approve the marked · x reject the marked · Esc close".to_string(),
        Style::new().fg(Color::Indexed(244)),
    ));
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_confirm_modal(f: &mut Frame, app: &App) {
    let Some(Modal::Confirm { text, .. }) = app.modal.as_ref() else {
        return;
    };
    let width = 66u16;
    let height = (text.len() as u16).saturating_add(2).max(6);
    let rect = centered(f.area(), width, height);
    f.render_widget(
        Block::bordered().title(" Confirm ").style(Style::new().fg(Color::Indexed(214))),
        rect,
    );
    let lines: Vec<Line> = text
        .iter()
        .map(|line| {
            if line.starts_with("[y]") {
                Line::styled(
                    line.clone(),
                    Style::new().fg(Color::Indexed(214)).bold(),
                )
            } else {
                Line::styled(line.clone(), Style::new().fg(Color::Indexed(252)))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner(rect));
}

fn draw_help_modal(f: &mut Frame) {
    let lines = vec![
        Line::styled("iris — key map", Style::new().bold()),
        Line::raw(String::new()),
        Line::raw("j/k or ↑/↓     move in the list"),
        Line::raw("h/l, Tab, 1/2  switch between the sections"),
        Line::raw("a              approve every reviewable environment of the run"),
        Line::raw("R              reject every reviewable environment of the run"),
        Line::raw("Enter          open the environment review of the run"),
        Line::raw("  space        toggle the marked environment in the review"),
        Line::raw("  Enter        approve the marked environments"),
        Line::raw("  x            reject the marked environments"),
        Line::raw("o              open the run in the browser"),
        Line::raw("c              cancel the run (recent section)"),
        Line::raw("r              rerun the run (recent section)"),
        Line::raw("f              rerun the failed jobs (recent section)"),
        Line::raw("g              refresh now (auto refresh runs every 30 s)"),
        Line::raw("?              this key map"),
        Line::raw("q or Esc       quit this TUI / close an open popup"),
        Line::raw(String::new()),
        Line::styled(
            "The desktop banner never approves. Approving happens here or on the website.",
            Style::new().fg(Color::Indexed(244)),
        ),
    ];
    let rect = centered(f.area(), 78, (lines.len() as u16) + 2);
    f.render_widget(
        Block::bordered().title(" key map ").style(Style::new().fg(Color::Indexed(214))),
        rect,
    );
    f.render_widget(Paragraph::new(lines), inner(rect));
}
