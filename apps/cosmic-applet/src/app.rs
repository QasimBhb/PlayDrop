//! The libcosmic side: panel button, popup, and the tasks behind each click.
//!
//! State lives in [`Model`]; this file only turns clicks into IPC calls and the model's
//! [`View`] into widgets. Every IPC call runs on the applet's tokio executor, never on the
//! UI thread, and every one of them has a timeout ([`daemon::timed`]).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use cosmic::app::Core;
use cosmic::applet::token::subscription::{
    activation_token_subscription, TokenRequest, TokenUpdate,
};
use cosmic::applet::{menu_button, padded_control};
use cosmic::cctk::sctk::reexports::calloop;
use cosmic::dialog::file_chooser;
use cosmic::iced::{window, Alignment, Length, Subscription};
use cosmic::surface;
use cosmic::widget::{self, button, divider, icon, text};
use cosmic::{Element, Task};
use playdrop_core::host::{PeerSummary, TransferSnapshot};
use playdrop_ipc::{Ok as Answer, Request, Status, MAX_SEND_ITEMS};
use tokio::sync::mpsc;

use crate::clipboard::{self, Clip};
use crate::daemon::{self, timed, Handle, Link, Wake};
use crate::launch;
use crate::model::{
    clipboard_outcome, Action, Busy, DeviceRow, Input, Model, PanelIcon, Refresh, StartAction,
    TransferRow, View, WindowAction,
};

/// The applet ID, also the desktop file's name.
pub const APP_ID: &str = "org.playdrop.PlayDrop.CosmicApplet";

/// The `PlayDrop` symbolic icon, drawn in the panel's colour.
const ICON: &[u8] =
    include_bytes!("../data/icons/hicolor/symbolic/apps/org.playdrop.PlayDrop-symbolic.svg");
/// The same drop, hollow and struck through: no daemon.
const ICON_OFFLINE: &[u8] = include_bytes!(
    "../data/icons/hicolor/symbolic/apps/org.playdrop.PlayDrop.CosmicApplet-offline-symbolic.svg"
);

/// How often finished transfers are aged out while nothing else happens.
const TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// The applet.
pub struct Applet {
    core: Core,
    model: Model,
    popup: Option<window::Id>,
    client: Option<Handle>,
    wake: Option<mpsc::UnboundedSender<Wake>>,
    token_tx: Option<calloop::channel::Sender<TokenRequest>>,
    /// Looked up when the popup opens: whether `playdrop-desktop` and `playdropd` exist.
    window_found: bool,
    daemon_found: bool,
    /// A password-manager clip waiting for "Send" or "Cancel", with its device (SR-24).
    /// Dropped when the popup closes.
    pending_clip: Option<(String, Clip)>,
    /// Bumped each time the popup opens or closes. A clipboard read carries the value from
    /// its click; a result from an earlier popup is dropped, so no clip outlives the popup
    /// it was read for.
    popup_gen: u64,
    /// `PLAYDROP_APPLET_OPEN_POPUP=1`: open the popup once the first connect attempt is
    /// answered, for screenshots and smoke tests without a pointer.
    open_popup_once: bool,
}

/// Files or a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// One or more files.
    Files,
    /// One folder (the daemon accepts folder descriptors).
    Folder,
}

/// What a file pick and send ended with.
#[derive(Debug, Clone)]
pub enum FilesOutcome {
    /// The daemon started transfer `id` for `paths`.
    Started {
        /// Transfer ID.
        id: String,
        /// What was sent, for the row's name. Never logged.
        paths: Vec<PathBuf>,
        /// Chosen items that were not local files and were left out.
        skipped: usize,
    },
    /// The user closed the chooser.
    Cancelled,
    /// Shown to the user.
    Failed(String),
}

/// What reading the clipboard for a send ended with.
#[derive(Debug, Clone)]
pub enum ClipRead {
    /// Plain text: send it now.
    Ready(Clip),
    /// A password manager marked it: ask first (SR-24).
    NeedsConfirm(Clip),
    /// Could not read; worded for the user.
    Failed(String),
}

/// Applet messages.
#[derive(Debug, Clone)]
pub enum Message {
    /// From the connection loop.
    Link(Link),
    /// Panel button.
    TogglePopup,
    /// A popup closed.
    PopupClosed(window::Id),
    /// Popup and tooltip surfaces.
    Surface(surface::Action<Message>),
    /// Wayland activation tokens for launching the window.
    Token(TokenUpdate),
    /// Periodic tidy-up.
    Tick,
    /// "Start `PlayDrop`".
    StartDaemon,
    /// "Open `PlayDrop`".
    OpenWindow,
    /// "Send clipboard" on a device (hex Device ID).
    SendClipboard(String),
    /// The clipboard was read for that device, from the click in popup generation `u64`.
    ClipboardRead(String, u64, ClipRead),
    /// "Send" on the password-manager question.
    ConfirmSensitive,
    /// "Cancel" on it.
    CancelSensitive,
    /// A clipboard send ended; the outcome is worded.
    ClipboardDone(String, String),
    /// "Send files..." or "Send folder..." on a device.
    SendFiles(String, Pick),
    /// Its outcome.
    FilesDone(String, FilesOutcome),
    /// "Stop" on the remote input line.
    StopInput,
    /// A request failed in a way the user should see.
    Notice(String),
    /// Fresh lists after an event asked for them.
    Peers(Vec<PeerSummary>),
    /// Fresh transfer list.
    Transfers(Vec<TransferSnapshot>),
    /// Fresh status.
    Status(Box<Status>),
    /// Nothing to do.
    Ignore,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Sends `text` to `device` and words the outcome. `label` is the device's label.
async fn push_clip(
    client: &playdrop_ipc::client::Client,
    device: &str,
    label: &str,
    clip: Clip,
) -> String {
    let request = Request::SendClipboard {
        peer: device.to_owned(),
        text: clip.text,
        sensitive: clip.sensitive,
    };
    match timed(client.request(request)).await {
        Ok(Answer::ClipboardPushed {
            sent_to,
            skipped_echo,
            skipped,
        }) => clipboard_outcome(label, !sent_to.is_empty(), skipped, skipped_echo),
        Ok(_) => "Clipboard not sent: the daemon gave an unexpected answer.".to_owned(),
        Err(e) => {
            tracing::info!(code = %e.code, "clipboard send refused");
            format!("Clipboard not sent: {}", e.message)
        }
    }
}

impl Applet {
    fn view_model(&self) -> View {
        self.model.view(self.daemon_found, self.window_found)
    }

    fn apply(&mut self, input: Input) -> Task<cosmic::Action<Message>> {
        let refresh = self.model.apply(input, now());
        self.sync_pending_clip();
        self.refresh(refresh)
    }

    /// The pending clip lives exactly as long as the model's question about it: when the
    /// model drops the question (device offline, unpaired, daemon gone), the clip goes too.
    fn sync_pending_clip(&mut self) {
        let asked = self.model.confirm_sensitive();
        if self
            .pending_clip
            .as_ref()
            .is_some_and(|(device, _)| Some(device.as_str()) != asked)
        {
            self.pending_clip = None;
        }
    }

    /// Fetches what the reducer asked for.
    fn refresh(&self, refresh: Refresh) -> Task<cosmic::Action<Message>> {
        let Some(Handle(client)) = self.client.clone() else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        if refresh.status {
            let c = client.clone();
            tasks.push(cosmic::task::future(async move {
                match daemon::load_status(&c).await {
                    Ok(s) => Message::Status(Box::new(s)),
                    Err(e) => {
                        tracing::debug!(code = %e.code, "status refresh failed");
                        Message::Ignore
                    }
                }
            }));
        }
        if refresh.peers {
            let c = client.clone();
            tasks.push(cosmic::task::future(async move {
                match daemon::load_peers(&c).await {
                    Ok(p) => Message::Peers(p),
                    Err(e) => {
                        tracing::debug!(code = %e.code, "peers refresh failed");
                        Message::Ignore
                    }
                }
            }));
        }
        if refresh.transfers {
            tasks.push(cosmic::task::future(async move {
                match daemon::load_transfers(&client).await {
                    Ok(t) => Message::Transfers(t),
                    Err(e) => {
                        tracing::debug!(code = %e.code, "transfers refresh failed");
                        Message::Ignore
                    }
                }
            }));
        }
        Task::batch(tasks)
    }

    fn on_link(&mut self, link: Link) -> Task<cosmic::Action<Message>> {
        match link {
            Link::Ready(tx) => {
                self.wake = Some(tx);
                Task::none()
            }
            Link::Connected(handle, snapshot) => {
                self.client = Some(handle);
                let snapshot = *snapshot;
                self.apply(Input::Connected {
                    status: snapshot.status,
                    peers: snapshot.peers,
                    transfers: snapshot.transfers,
                })
            }
            Link::ConnectFailed(p) => {
                self.client = None;
                self.pending_clip = None;
                self.apply(Input::ConnectFailed(p))
            }
            Link::StartFailed(reason) => {
                self.client = None;
                self.apply(Input::StartFailed(reason))
            }
            Link::Event(e) => self.apply(Input::Event(e)),
            Link::Lagged => self.refresh(Refresh::ALL),
            Link::Disconnected => {
                self.client = None;
                self.pending_clip = None;
                self.apply(Input::Disconnected)
            }
        }
    }

    fn close_popup_state(&mut self) {
        self.popup_gen = self.popup_gen.wrapping_add(1);
        self.model.clear_notice();
        self.pending_clip = None;
        self.model.set_confirm_sensitive(None);
    }

    fn toggle_popup(&mut self) -> Task<cosmic::Action<Message>> {
        if let Some(p) = self.popup.take() {
            self.close_popup_state();
            return cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(p));
        }
        self.popup_gen = self.popup_gen.wrapping_add(1);
        // Look the programs up now, so an install made while the applet ran is seen.
        self.window_found = launch::find(launch::WINDOW_BIN).is_some();
        self.daemon_found = launch::find(launch::DAEMON_BIN).is_some();
        // A good moment to try again rather than wait out the back-off. This never starts
        // the daemon; only "Start PlayDrop" does.
        if !self.model.is_running() {
            if let Some(w) = &self.wake {
                let _ = w.send(Wake::Retry);
            }
        }
        cosmic::surface::surface_task(cosmic::surface::action::app_popup(
            |_| cosmic::surface::action::LiveSettings::default(),
            |app: &mut Applet| {
                let id = window::Id::unique();
                app.popup = Some(id);
                let parent = app.core.main_window_id().unwrap_or(window::Id::RESERVED);
                app.core
                    .applet
                    .get_popup_settings(parent, id, None, None, None)
            },
            None,
        ))
    }

    fn send_clipboard(&mut self, device: String) -> Task<cosmic::Action<Message>> {
        if self.client.is_none() {
            return Task::none();
        }
        self.model.set_busy(&device, Busy::Clipboard, true);
        let generation = self.popup_gen;
        cosmic::task::future(async move {
            let read = match tokio::task::spawn_blocking(clipboard::read_text).await {
                Ok(Ok(c)) if c.sensitive => ClipRead::NeedsConfirm(c),
                Ok(Ok(c)) => ClipRead::Ready(c),
                Ok(Err(e)) => ClipRead::Failed(e.message()),
                Err(_) => ClipRead::Failed("Could not read the clipboard.".to_owned()),
            };
            Message::ClipboardRead(device, generation, read)
        })
    }

    fn push(&mut self, device: String, clip: Clip) -> Task<cosmic::Action<Message>> {
        let Some(Handle(client)) = self.client.clone() else {
            self.model.set_busy(&device, Busy::Clipboard, false);
            return Task::none();
        };
        self.model.set_busy(&device, Busy::Clipboard, true);
        let label = self.model.label_of(&device);
        cosmic::task::future(async move {
            let outcome = push_clip(&client, &device, &label, clip).await;
            Message::ClipboardDone(device, outcome)
        })
    }

    fn send_files(&mut self, device: String, pick: Pick) -> Task<cosmic::Action<Message>> {
        let Some(Handle(client)) = self.client.clone() else {
            return Task::none();
        };
        self.model.set_busy(&device, Busy::Files, true);
        let label = self.model.label_of(&device);
        cosmic::task::future(async move {
            let dialog = file_chooser::open::Dialog::new()
                .title(match pick {
                    Pick::Files => format!("Send files to {label}"),
                    Pick::Folder => format!("Send a folder to {label}"),
                })
                .accept_label("Send");
            // No timeout here: the user may take as long as they like to choose.
            let chosen = match pick {
                Pick::Files => dialog.open_files().await.map(|r| r.urls().to_vec()),
                Pick::Folder => dialog.open_folder().await.map(|r| vec![r.url().clone()]),
            };
            let urls = match chosen {
                Ok(urls) => urls,
                Err(file_chooser::Error::Cancelled) => {
                    return Message::FilesDone(device, FilesOutcome::Cancelled)
                }
                Err(e) => {
                    tracing::info!(error = %e, "file chooser failed");
                    return Message::FilesDone(
                        device,
                        FilesOutcome::Failed(format!("Could not open the file chooser: {e}")),
                    );
                }
            };
            let total = urls.len();
            let paths: Vec<PathBuf> = urls.iter().filter_map(|u| u.to_file_path().ok()).collect();
            let skipped = total - paths.len();
            if paths.is_empty() {
                return Message::FilesDone(
                    device,
                    FilesOutcome::Failed(
                        "Nothing sent: only files on this computer can be sent.".to_owned(),
                    ),
                );
            }
            if paths.len() > MAX_SEND_ITEMS {
                return Message::FilesDone(
                    device,
                    FilesOutcome::Failed(format!(
                        "Nothing sent: choose at most {MAX_SEND_ITEMS} files at a time, or use \
                         \"Send folder...\"."
                    )),
                );
            }
            // Travels as file descriptors on Linux, like the window's sends (SR-26).
            let outcome = match timed(client.send_files(&device, paths.clone())).await {
                Ok(Answer::TransferStarted { id }) => FilesOutcome::Started { id, paths, skipped },
                Ok(_) => FilesOutcome::Failed("The daemon gave an unexpected answer.".to_owned()),
                Err(e) => {
                    tracing::info!(code = %e.code, "send refused");
                    FilesOutcome::Failed(format!("Not sent: {}", e.message))
                }
            };
            Message::FilesDone(device, outcome)
        })
    }

    fn stop_input(&self) -> Task<cosmic::Action<Message>> {
        let Some(Handle(client)) = self.client.clone() else {
            return Task::none();
        };
        cosmic::task::future(async move {
            match timed(client.request(Request::StopInput)).await {
                Ok(_) => Message::Notice("Remote input stopped.".to_owned()),
                Err(e) => Message::Notice(format!("Could not stop remote input: {}", e.message)),
            }
        })
    }

    fn open_window(&mut self) -> Task<cosmic::Action<Message>> {
        let Some(bin) = launch::find(launch::WINDOW_BIN) else {
            self.window_found = false;
            return Task::none();
        };
        if let Some(tx) = &self.token_tx {
            let request = TokenRequest {
                app_id: APP_ID.to_owned(),
                exec: bin.to_string_lossy().into_owned(),
            };
            if tx.send(request).is_ok() {
                return Task::none();
            }
        }
        launch_window(bin, None)
    }

    fn files_done(&mut self, device: &str, outcome: FilesOutcome) -> Task<cosmic::Action<Message>> {
        self.model.set_busy(device, Busy::Files, false);
        match outcome {
            FilesOutcome::Started { id, paths, skipped } => {
                let label = self.model.label_of(device);
                self.model.note_sent(&id, &paths);
                let mut notice = match paths.len() {
                    1 => format!("Sending 1 item to {label}."),
                    n => format!("Sending {n} items to {label}."),
                };
                if skipped > 0 {
                    let _ = write!(
                        notice,
                        " {skipped} {} left out: only files on this computer can be sent.",
                        if skipped == 1 {
                            "item was"
                        } else {
                            "items were"
                        }
                    );
                }
                self.model.set_notice(notice);
                return self.refresh(Refresh::TRANSFERS);
            }
            FilesOutcome::Cancelled => {}
            FilesOutcome::Failed(m) => self.model.set_notice(m),
        }
        Task::none()
    }
}

fn launch_window(bin: PathBuf, token: Option<String>) -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async move {
        if launch::spawn(bin, token).await {
            Message::Ignore
        } else {
            Message::Notice("The PlayDrop window could not be started.".to_owned())
        }
    })
}

impl cosmic::Application for Applet {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: ()) -> (Self, Task<cosmic::Action<Message>>) {
        let applet = Self {
            core,
            model: Model::new(),
            popup: None,
            client: None,
            wake: None,
            token_tx: None,
            window_found: launch::find(launch::WINDOW_BIN).is_some(),
            daemon_found: launch::find(launch::DAEMON_BIN).is_some(),
            pending_clip: None,
            popup_gen: 0,
            open_popup_once: std::env::var_os("PLAYDROP_APPLET_OPEN_POPUP")
                .is_some_and(|v| v == "1"),
        };
        (applet, Task::none())
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Link(link) => {
                let first_answer = matches!(
                    link,
                    Link::Connected(..) | Link::ConnectFailed(_) | Link::StartFailed(_)
                );
                let task = self.on_link(link);
                if first_answer && std::mem::take(&mut self.open_popup_once) && self.popup.is_none()
                {
                    return Task::batch([task, self.toggle_popup()]);
                }
                return task;
            }
            Message::TogglePopup => return self.toggle_popup(),
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                    self.close_popup_state();
                }
            }
            Message::Surface(a) => return cosmic::task::message(cosmic::Action::Surface(a)),
            Message::Token(u) => match u {
                TokenUpdate::Init(tx) => self.token_tx = Some(tx),
                TokenUpdate::Finished => self.token_tx = None,
                TokenUpdate::ActivationToken { token, exec } => {
                    return launch_window(PathBuf::from(exec), token);
                }
            },
            Message::Tick => self.model.tick(now()),
            Message::StartDaemon => {
                return match &self.wake {
                    Some(w) if w.send(Wake::Start).is_ok() => self.apply(Input::StartRequested),
                    _ => self.apply(Input::StartFailed(
                        "the applet's connection task is not running".to_owned(),
                    )),
                };
            }
            Message::OpenWindow => return self.open_window(),
            Message::SendClipboard(device) => return self.send_clipboard(device),
            Message::ClipboardRead(device, generation, read) => {
                self.model.set_busy(&device, Busy::Clipboard, false);
                if self.popup.is_none() || generation != self.popup_gen {
                    // Read for a popup that has closed since: drop it, clip and all.
                    return Task::none();
                }
                match read {
                    ClipRead::Ready(clip) => return self.push(device, clip),
                    ClipRead::NeedsConfirm(clip) => {
                        self.model.set_confirm_sensitive(Some(&device));
                        self.pending_clip = Some((device, clip));
                        // The device may have gone while the clipboard was read.
                        self.sync_pending_clip();
                    }
                    ClipRead::Failed(m) => self.model.set_notice(m),
                }
            }
            Message::ConfirmSensitive => {
                self.model.set_confirm_sensitive(None);
                if let Some((device, clip)) = self.pending_clip.take() {
                    // Sent marked sensitive, so the phone keeps it out of its history.
                    return self.push(device, clip);
                }
            }
            Message::CancelSensitive => {
                self.model.set_confirm_sensitive(None);
                self.pending_clip = None;
                self.model.set_notice("Clipboard not sent.");
            }
            Message::ClipboardDone(device, outcome) => {
                self.model.set_busy(&device, Busy::Clipboard, false);
                self.model.set_notice(outcome);
            }
            Message::SendFiles(device, pick) => return self.send_files(device, pick),
            Message::FilesDone(device, outcome) => return self.files_done(&device, outcome),
            Message::StopInput => return self.stop_input(),
            Message::Notice(n) => self.model.set_notice(n),
            Message::Peers(p) => return self.apply(Input::Peers(p)),
            Message::Transfers(t) => return self.apply(Input::Transfers(t)),
            Message::Status(s) => return self.apply(Input::Status(s)),
            Message::Ignore => {}
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let v = self.view_model();
        let bytes = match v.icon {
            PanelIcon::Normal => ICON,
            PanelIcon::Offline => ICON_OFFLINE,
        };
        let handle = icon::from_svg_bytes(bytes).symbolic(true);
        let btn = self
            .core
            .applet
            .icon_button_from_handle(handle)
            .on_press_down(Message::TogglePopup);
        self.core
            .applet
            .applet_tooltip::<Message>(btn, v.tooltip, self.popup.is_some(), Message::Surface, None)
            .into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if self.popup != Some(id) {
            return text("").into();
        }
        let v = self.view_model();
        let spacing = cosmic::theme::active().cosmic().spacing;
        let mut content = widget::Column::new()
            .spacing(spacing.space_xxs)
            .padding([8, 0]);

        // Daemon state, start, notices.
        let mut header = widget::Column::new()
            .spacing(spacing.space_xxs)
            .push(text::heading(v.status_line.clone()));
        match &v.start {
            StartAction::Available => {
                header =
                    header.push(button::suggested("Start PlayDrop").on_press(Message::StartDaemon));
            }
            StartAction::Unavailable(why) => header = header.push(text::caption(why.clone())),
            StartAction::Hidden => {}
        }
        if let Some(note) = &v.locked_note {
            header = header.push(text::caption(note.clone()));
        }
        if let Some(n) = &v.notice {
            header = header.push(text::caption(n.clone()));
        }
        content = content.push(padded_control(header));

        // Remote input kill switch (SR-25).
        if let Some(banner) = &v.input_banner {
            content = content.push(padded_control(
                widget::Row::new()
                    .spacing(spacing.space_xs)
                    .align_y(Alignment::Center)
                    .push(text::body(banner.clone()).width(Length::Fill))
                    .push(button::destructive("Stop").on_press(Message::StopInput)),
            ));
        }

        if v.show_devices {
            content = content.push(sep(spacing.space_xxs, spacing.space_s));
            content = content.push(padded_control(text::caption_heading("Devices")));
            if v.devices.is_empty() {
                content = content.push(padded_control(text::caption(
                    "Pair a device from the PlayDrop window.",
                )));
            }
            for d in &v.devices {
                content = content.push(device_row(d, spacing.space_xxs));
            }
        }

        if !v.transfers.is_empty() {
            content = content.push(sep(spacing.space_xxs, spacing.space_s));
            content = content.push(padded_control(text::caption_heading("Transfers")));
            for t in &v.transfers {
                content = content.push(transfer_row(t, spacing.space_xxxs));
            }
        }

        content = content.push(sep(spacing.space_xxs, spacing.space_s));
        match &v.open_window {
            WindowAction::Available => {
                content = content
                    .push(menu_button(text::body("Open PlayDrop")).on_press(Message::OpenWindow));
            }
            WindowAction::Unavailable(why) => {
                content = content.push(padded_control(text::caption(why.clone())));
            }
        }

        self.core.applet.popup_container(content).into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run(daemon::run).map(Message::Link),
            activation_token_subscription(0).map(Message::Token),
            cosmic::iced::time::every(TICK).map(|_| Message::Tick),
        ])
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

fn sep<'a>(vertical: u16, horizontal: u16) -> Element<'a, Message> {
    padded_control(divider::horizontal::default())
        .padding([vertical, horizontal])
        .into()
}

/// A row of buttons, or nothing when empty.
fn buttons(gap: u16, list: Vec<Element<'static, Message>>) -> Option<Element<'static, Message>> {
    if list.is_empty() {
        return None;
    }
    let mut row = widget::Row::new().spacing(gap * 2);
    for b in list {
        row = row.push(b);
    }
    Some(row.into())
}

fn device_row(d: &DeviceRow, gap: u16) -> Element<'static, Message> {
    let mut col = widget::Column::new()
        .spacing(gap)
        .push(text::body(d.title.clone()))
        .push(text::caption(d.detail.clone()));
    // Reasons and busy notes go on their own lines so they are never cut off.
    let mut notes: Vec<Element<'static, Message>> = Vec::new();

    if let Some(question) = &d.confirm {
        col = col.push(text::caption(question.clone()));
        if let Some(row) = buttons(
            gap,
            vec![
                button::suggested("Send")
                    .on_press(Message::ConfirmSensitive)
                    .into(),
                button::standard("Cancel")
                    .on_press(Message::CancelSensitive)
                    .into(),
            ],
        ) {
            col = col.push(row);
        }
    }
    match &d.clipboard {
        Action::Available => {
            // One click from the open popup.
            col = col.push(
                button::standard("Send clipboard")
                    .on_press(Message::SendClipboard(d.device_id.clone())),
            );
        }
        Action::Unavailable(why) | Action::Busy(why) => {
            notes.push(text::caption(why.clone()).into());
        }
        Action::Hidden => {}
    }
    match &d.files {
        Action::Available => {
            if let Some(row) = buttons(
                gap,
                vec![
                    button::standard("Send files...")
                        .on_press(Message::SendFiles(d.device_id.clone(), Pick::Files))
                        .into(),
                    button::standard("Send folder...")
                        .on_press(Message::SendFiles(d.device_id.clone(), Pick::Folder))
                        .into(),
                ],
            ) {
                col = col.push(row);
            }
        }
        Action::Unavailable(why) | Action::Busy(why) => {
            notes.push(text::caption(why.clone()).into());
        }
        Action::Hidden => {}
    }
    for n in notes {
        col = col.push(n);
    }
    padded_control(col).into()
}

fn transfer_row(t: &TransferRow, gap: u16) -> Element<'static, Message> {
    let mut col = widget::Column::new()
        .spacing(gap)
        .push(text::body(t.title.clone()))
        .push(text::caption(t.detail.clone()));
    if let Some(p) = t.progress {
        col = col.push(widget::determinate_linear(p).width(Length::Fill));
    }
    padded_control(col).into()
}
