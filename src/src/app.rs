//! State + update + view untuk TabVoice (iced 0.13 function-based API).
//!
//! Phase 5: floating pill UI (Recording / Processing / Done mode) yang drain
//! `AppEvent` dari background threads lewat subscription Tick 60fps.

use std::collections::VecDeque;
use std::sync::{mpsc::Receiver, Arc};
use std::time::Duration;

use iced::time;
use iced::widget::{button, checkbox, column, container, mouse_area, pick_list, row, text, text_input};
use iced::{Element, Subscription, Task};

use crate::events::{AppEvent, TrayAction};
use crate::state::AppState;
use crate::view::{
    blinking_caret, mic_icon, pill_background, pill_row, spinner_dots, waveform_canvas,
    AMP_HISTORY_LEN,
};

/// State utama aplikasi.
///
/// `ui` adalah state yang hanya relevan untuk UI thread (mode, animasi).
/// `state` adalah shared state lintas thread (model, recorder, settings).
/// `event_rx` adalah receiver untuk `AppEvent` dari audio/hotkey/tray threads.
pub struct TabVoice {
    /// UI-local state: mode, text, waveform, blink frame, fade timer.
    pub ui: UiState,
    /// Shared state lintas thread.
    pub state: Arc<AppState>,
    /// Receiver untuk AppEvent (drain setiap Tick 60fps).
    pub event_rx: Receiver<AppEvent>,
    /// Sender untuk men-trigger event secara programmatis dari UI.
    pub event_tx: crate::events::EventSender,
}

/// State khusus UI thread. Tidak dishare ke background worker.
#[derive(Debug)]
pub struct UiState {
    /// Mode pill saat ini.
    pub mode: UiMode,
    /// Teks yang ditampilkan (untuk mode Done).
    pub text: String,
    /// Rolling history amplitude (0.0 - 1.0) untuk waveform bars.
    pub amp_history: VecDeque<f32>,
    /// Frame counter, increment setiap Tick (untuk blink animation).
    pub frame: u64,
    /// Sisa frame sebelum fade-out (untuk Done / Error mode).
    pub fade_timer: u32,
    /// ID window utama untuk melakukan drag.
    pub window_id: Option<iced::window::Id>,
    /// Temporary state untuk settings panel.
    pub temp_model_path: String,
    /// Temporary language code untuk settings panel.
    pub temp_language: String,
    /// Temporary hotkey string untuk settings panel.
    pub temp_hotkey: String,
    /// Temporary auto-paste boolean untuk settings panel.
    pub temp_paste: bool,
    /// Daftar nama file model (.bin) yang tersedia di folder models.
    pub available_models: Vec<String>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            mode: UiMode::Idle,
            text: String::new(),
            amp_history: VecDeque::with_capacity(AMP_HISTORY_LEN),
            frame: 0,
            fade_timer: 0,
            window_id: None,
            temp_model_path: String::new(),
            temp_language: String::new(),
            temp_hotkey: String::new(),
            temp_paste: true,
            available_models: Vec::new(),
        }
    }
}

/// Mode pill UI. Drive render di `view()`.
#[derive(Debug, Clone, PartialEq)]
pub enum UiMode {
    /// Pill hidden (window tetap transparan via settings).
    Idle,
    /// Sedang merekam: waveform + blinking caret.
    Recording,
    /// Sedang transcribe: spinner.
    Processing,
    /// Selesai: tampilkan text, lalu fade out ke Idle.
    Done,
    /// Halaman pengaturan (Settings panel).
    Settings,
}

/// Variant message internal iced.
#[derive(Debug, Clone)]
pub enum Message {
    /// Frame tick 60fps: drain event_rx, update animasi.
    Tick,
    /// Event langsung (untuk testability — biasanya dipicu Tick).
    Event(AppEvent),
    /// Event window dari iced (mendeteksi id window).
    WindowEvent(iced::window::Id, iced::window::Event),
    /// Perintah untuk drag window utama.
    DragWindow,
    /// Perintah untuk men-toggle perekaman (opsi 1 tap).
    ToggleRecording,
    /// Event ketika teks path model diubah di UI settings.
    ModelPathChanged(String),
    /// Event ketika teks bahasa diubah di UI settings.
    LanguageChanged(String),
    /// Event ketika teks hotkey diubah di UI settings.
    HotkeyChanged(String),
    /// Event ketika toggle auto-paste diubah di UI settings.
    PasteToggled(bool),
    /// Simpan pengaturan dan kembalikan window ke mode normal.
    SaveSettings,
    /// Batalkan pengaturan dan kembalikan window ke mode normal.
    CancelSettings,
}

/// Flags untuk inisialisasi `TabVoice` lewat `run_with`.
pub struct AppFlags {
    /// Receiver untuk `AppEvent` dari background threads.
    pub event_rx: Receiver<AppEvent>,
    /// Sender untuk men-trigger event secara programmatis dari UI.
    pub event_tx: crate::events::EventSender,
    /// Shared state lintas thread.
    pub state: Arc<AppState>,
}

impl TabVoice {
    /// Constructor dari `AppFlags`. Dipanggil dari `main::run_with`.
    pub fn new(flags: AppFlags) -> Self {
        Self {
            ui: UiState::default(),
            state: flags.state,
            event_rx: flags.event_rx,
            event_tx: flags.event_tx,
        }
    }
}

/// Update function (function-based iced 0.13 API).
pub fn update(state: &mut TabVoice, message: Message) -> Task<Message> {
    match message {
        Message::Tick => {
            tick(state)
        }
        Message::Event(ev) => {
            apply_event(state, ev)
        }
        Message::WindowEvent(id, _event) => {
            if state.ui.window_id.is_none() {
                state.ui.window_id = Some(id);
            }
            Task::none()
        }
        Message::DragWindow => {
            if let Some(id) = state.ui.window_id {
                iced::window::drag(id)
            } else {
                Task::none()
            }
        }
        Message::ToggleRecording => {
            if state.ui.mode == UiMode::Recording {
                crate::hotkey::handle_release(&state.event_tx, &state.state);
            } else {
                crate::hotkey::handle_press(&state.event_tx, &state.state);
            }
            Task::none()
        }
        Message::ModelPathChanged(val) => {
            state.ui.temp_model_path = val;
            Task::none()
        }
        Message::LanguageChanged(val) => {
            state.ui.temp_language = val;
            Task::none()
        }
        Message::HotkeyChanged(val) => {
            state.ui.temp_hotkey = val;
            Task::none()
        }
        Message::PasteToggled(val) => {
            state.ui.temp_paste = val;
            Task::none()
        }
        Message::SaveSettings => {
            let new_settings = crate::settings::Settings {
                model_path: std::path::PathBuf::from("models").join(&state.ui.temp_model_path),
                language: if state.ui.temp_language == "auto" || state.ui.temp_language.trim().is_empty() { None } else { Some(state.ui.temp_language.trim().to_string()) },
                hotkey: state.ui.temp_hotkey.clone(),
                paste_on_release: state.ui.temp_paste,
            };

            if let Err(e) = crate::settings::save(&new_settings) {
                log::error!("Gagal menyimpan settings: {e}");
            }

            *state.state.settings.lock().unwrap() = new_settings;

            state.ui.mode = UiMode::Idle;
            if let Some(id) = state.ui.window_id {
                iced::window::resize(id, iced::Size::new(600.0, 80.0))
            } else {
                Task::none()
            }
        }
        Message::CancelSettings => {
            state.ui.mode = UiMode::Idle;
            if let Some(id) = state.ui.window_id {
                iced::window::resize(id, iced::Size::new(600.0, 80.0))
            } else {
                Task::none()
            }
        }
    }
}

/// Handler Tick: drain semua event yang tersedia, increment frame, decrement fade.
fn tick(state: &mut TabVoice) -> Task<Message> {
    state.ui.frame = state.ui.frame.wrapping_add(1);

    let mut tasks = Vec::new();

    // Drain event_rx tanpa memblok. `try_recv` return Err kalau empty / disconnected.
    while let Ok(ev) = state.event_rx.try_recv() {
        let task = apply_event(state, ev);
        tasks.push(task);
    }

    // Decrement fade timer kalau aktif.
    if state.ui.fade_timer > 0 {
        state.ui.fade_timer = state.ui.fade_timer.saturating_sub(1);
        // Kalau sudah 0 dan mode adalah Done, kembali ke Idle (pill hide).
        if state.ui.fade_timer == 0 && state.ui.mode == UiMode::Done {
            state.ui.mode = UiMode::Idle;
            state.ui.text.clear();
            state.ui.amp_history.clear();
        }
    }

    Task::batch(tasks)
}

/// Terapkan satu `AppEvent` ke UiState. Dipanggil dari Tick loop atau `Message::Event`.
fn apply_event(state: &mut TabVoice, ev: AppEvent) -> Task<Message> {
    match ev {
        AppEvent::Amplitude { value } => {
            // Push amplitude ke rolling history (cap ke AMP_HISTORY_LEN).
            let amp = value.clamp(0.0, 1.0);
            if state.ui.amp_history.len() >= AMP_HISTORY_LEN {
                state.ui.amp_history.pop_front();
            }
            state.ui.amp_history.push_back(amp);

            // Pastikan mode Recording (kalau sedang Idle/Done, kembali ke Recording, tapi abaikan jika di Settings).
            if state.ui.mode != UiMode::Recording && state.ui.mode != UiMode::Settings {
                state.ui.mode = UiMode::Recording;
                state.ui.text.clear();
                state.ui.fade_timer = 0;
            }
            Task::none()
        }
        AppEvent::Done { text } => {
            state.ui.text = text;
            state.ui.mode = UiMode::Done;
            // 90 frame @60fps = 1.5 detik.
            state.ui.fade_timer = 90;
            state.ui.amp_history.clear();
            Task::none()
        }
        AppEvent::Error { message } => {
            state.ui.text = format!("Error: {}", message);
            state.ui.mode = UiMode::Done;
            // 120 frame @60fps = 2 detik (lebih lama biar user bisa baca).
            state.ui.fade_timer = 120;
            state.ui.amp_history.clear();
            Task::none()
        }
        AppEvent::HotkeyPressed | AppEvent::HotkeyReleased => {
            // Ditangani oleh hotkey.rs sendiri (audio start/stop).
            Task::none()
        }
        AppEvent::TrayAction(action) => handle_tray(state, action),
    }
}

/// Aksi tray: Quit -> exit, OpenSettings -> buka Settings panel dan resize window.
fn handle_tray(state: &mut TabVoice, action: TrayAction) -> Task<Message> {
    match action {
        TrayAction::ReloadModel => {
            log::info!("tray: ReloadModel (handled in Phase 6)");
            Task::none()
        }
        TrayAction::OpenSettings => {
            state.ui.mode = UiMode::Settings;
            state.ui.available_models = list_available_models();
            let current = state.state.settings.lock().unwrap().clone();
            
            // Ambil hanya nama file model saat ini agar cocok dengan dropdown
            let model_filename = current.model_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("ggml-base.bin")
                .to_string();
            state.ui.temp_model_path = model_filename;
            
            state.ui.temp_language = current.language.clone().unwrap_or_else(|| "auto".to_string());
            state.ui.temp_hotkey = current.hotkey.clone();
            state.ui.temp_paste = current.paste_on_release;
            
            if let Some(id) = state.ui.window_id {
                iced::window::resize(id, iced::Size::new(480.0, 260.0))
            } else {
                Task::none()
            }
        }
        TrayAction::Quit => {
            log::info!("tray: Quit");
            std::process::exit(0);
        }
    }
}

/// Subscription 60fps + window events.
pub fn subscription(_state: &TabVoice) -> Subscription<Message> {
    Subscription::batch(vec![
        time::every(Duration::from_millis(16)).map(|_| Message::Tick),
        iced::window::events().map(|(id, event)| Message::WindowEvent(id, event)),
    ])
}

/// View function — render pill sesuai `state.ui.mode` dan posisikan di tengah window.
pub fn view(state: &TabVoice) -> Element<'_, Message> {
    let content = match state.ui.mode {
        UiMode::Idle => idle_view(state),
        UiMode::Recording => recording_view(state),
        UiMode::Processing => processing_view(state),
        UiMode::Done => done_view(state),
        UiMode::Settings => settings_view(state),
    };

    container(content)
        .width(iced::Length::Fill)
        .height(iced::Length::Fill)
        .align_x(iced::alignment::Horizontal::Center)
        .align_y(iced::alignment::Vertical::Center)
        .style(|_theme| container::Style {
            background: None,
            text_color: None,
            border: iced::Border::default(),
            shadow: iced::Shadow::default(),
        })
        .into()
}

/// Idle: persistent circular bubble berisi mic icon yang bisa di-drag, mic berfungsi sebagai tombol tap.
fn idle_view(_state: &TabVoice) -> Element<'_, Message> {
    let mic = mic_icon(iced::Color::from_rgba(1.0, 1.0, 1.0, 0.45), 18.0);
    let mic_btn = button(mic)
        .on_press(Message::ToggleRecording)
        .style(|_theme, _status| button::Style {
            background: None,
            border: iced::Border::default(),
            shadow: iced::Shadow::default(),
            ..Default::default()
        });

    let row = pill_row(vec![mic_btn.into()]);

    let pill = container(row)
        .padding([8, 12])
        .style(|_theme| pill_background());

    mouse_area(pill)
        .on_press(Message::DragWindow)
        .into()
}

/// Recording: mic icon + waveform + blinking caret yang bisa di-drag, mic berfungsi sebagai tombol tap.
fn recording_view(state: &TabVoice) -> Element<'_, Message> {
    let mic = mic_icon(iced::Color::from_rgba(0.40, 0.85, 1.00, 0.95), 18.0);
    let mic_btn = button(mic)
        .on_press(Message::ToggleRecording)
        .style(|_theme, _status| button::Style {
            background: None,
            border: iced::Border::default(),
            shadow: iced::Shadow::default(),
            ..Default::default()
        });

    let wave = waveform_canvas(&state.ui.amp_history);
    let caret = blinking_caret(state.ui.frame);

    let row = pill_row(vec![mic_btn.into(), wave.into(), caret]);

    let pill = container(row)
        .padding([8, 16])
        .max_width(480)
        .style(|_theme| pill_background());

    mouse_area(pill)
        .on_press(Message::DragWindow)
        .into()
}

/// Processing: mic icon + spinner (3 titik animasi) yang bisa di-drag.
fn processing_view(state: &TabVoice) -> Element<'_, Message> {
    let mic = mic_icon(iced::Color::from_rgba(0.35, 0.65, 1.0, 0.85), 18.0);
    let spinner = spinner_dots(state.ui.frame);
    let row = pill_row(vec![mic, spinner]);

    let pill = container(row)
        .padding([8, 16])
        .style(|_theme| pill_background());

    mouse_area(pill)
        .on_press(Message::DragWindow)
        .into()
}

/// Done: tampilkan text hasil transkrip / pesan error, lalu fade.
fn done_view(state: &TabVoice) -> Element<'_, Message> {
    // Truncate teks panjang biar pill tidak overflow.
    let display = if state.ui.text.chars().count() > 80 {
        let truncated: String = state.ui.text.chars().take(80).collect();
        format!("{}…", truncated)
    } else {
        state.ui.text.clone()
    };

    let is_error = display.starts_with("Error: ");
    let text_color = if is_error {
        iced::Color::from_rgb(1.0, 0.4, 0.4) // Soft red coral untuk error
    } else {
        iced::Color::from_rgba(0.95, 0.96, 0.98, 0.95)
    };

    let label = text(display).size(13).color(text_color);
    let row = pill_row(vec![label.into()]);

    let pill = container(row)
        .padding([8, 16])
        .max_width(480)
        .style(|_theme| pill_background());

    mouse_area(pill)
        .on_press(Message::DragWindow)
        .into()
}

/// Settings: Form UI untuk mengonfigurasi aplikasi.
fn settings_view(state: &TabVoice) -> Element<'_, Message> {
    use iced::Length;
    use iced::Color;
    use iced::Border;

    let label_width = Length::Fixed(110.0);

    let header = text("TabVoice Settings")
        .size(15)
        .color(Color::from_rgb(0.35, 0.65, 1.0));

    // Field 1: Model Dropdown
    let model_label = container(text("Model").size(12).color(Color::from_rgb(0.75, 0.75, 0.75)))
        .width(label_width)
        .align_y(iced::alignment::Vertical::Center);
    
    let selected_model = Some(state.ui.temp_model_path.clone());
    
    let model_input = pick_list(
        &state.ui.available_models[..],
        selected_model,
        Message::ModelPathChanged,
    )
    .padding(6)
    .text_size(12)
    .style(|_theme, _status| pick_list::Style {
        background: Color::from_rgba(0.1, 0.1, 0.15, 0.8).into(),
        border: Border {
            color: Color::from_rgba(0.35, 0.65, 1.0, 0.25),
            width: 1.0,
            radius: 4.0.into(),
        },
        text_color: Color::WHITE,
        placeholder_color: Color::from_rgb(0.45, 0.45, 0.45),
        handle_color: Color::from_rgb(0.35, 0.65, 1.0),
    });
    
    let model_row = row(vec![model_label.into(), model_input.into()])
        .align_y(iced::alignment::Vertical::Center);

    // Field 2: Language Hint
    let lang_label = container(text("Language Hint").size(12).color(Color::from_rgb(0.75, 0.75, 0.75)))
        .width(label_width)
        .align_y(iced::alignment::Vertical::Center);
    let lang_input = text_input(
        "auto",
        &state.ui.temp_language,
    )
    .on_input(Message::LanguageChanged)
    .padding(6)
    .size(12)
    .style(|_theme, _status| text_input::Style {
        background: Color::from_rgba(0.1, 0.1, 0.15, 0.8).into(),
        border: Border {
            color: Color::from_rgba(0.35, 0.65, 1.0, 0.25),
            width: 1.0,
            radius: 4.0.into(),
        },
        value: Color::WHITE,
        placeholder: Color::from_rgb(0.45, 0.45, 0.45),
        selection: Color::from_rgba(0.35, 0.65, 1.0, 0.35),
        icon: Color::TRANSPARENT,
    });
    let lang_row = row(vec![lang_label.into(), lang_input.into()])
        .align_y(iced::alignment::Vertical::Center);

    // Field 3: Hotkey
    let hotkey_label = container(text("Hotkey").size(12).color(Color::from_rgb(0.75, 0.75, 0.75)))
        .width(label_width)
        .align_y(iced::alignment::Vertical::Center);
    let hotkey_input = text_input(
        "Ctrl+Shift+Space",
        &state.ui.temp_hotkey,
    )
    .on_input(Message::HotkeyChanged)
    .padding(6)
    .size(12)
    .style(|_theme, _status| text_input::Style {
        background: Color::from_rgba(0.1, 0.1, 0.15, 0.8).into(),
        border: Border {
            color: Color::from_rgba(0.35, 0.65, 1.0, 0.25),
            width: 1.0,
            radius: 4.0.into(),
        },
        value: Color::WHITE,
        placeholder: Color::from_rgb(0.45, 0.45, 0.45),
        selection: Color::from_rgba(0.35, 0.65, 1.0, 0.35),
        icon: Color::TRANSPARENT,
    });
    let hotkey_row = row(vec![hotkey_label.into(), hotkey_input.into()])
        .align_y(iced::alignment::Vertical::Center);

    // Field 4: Auto-paste Toggle
    let paste_checkbox = checkbox("Auto-paste on release", state.ui.temp_paste)
        .on_toggle(Message::PasteToggled)
        .size(14)
        .text_size(12)
        .style(|_theme, _status| checkbox::Style {
            background: Color::from_rgba(0.1, 0.1, 0.15, 0.8).into(),
            icon_color: Color::from_rgb(0.35, 0.65, 1.0),
            border: Border {
                color: Color::from_rgba(0.35, 0.65, 1.0, 0.25),
                width: 1.0,
                radius: 3.0.into(),
            },
            text_color: Some(Color::from_rgb(0.75, 0.75, 0.75)),
        });
    let paste_row = row(vec![
        container(iced::widget::Space::new(Length::Fixed(0.0), Length::Fixed(0.0))).width(label_width).into(),
        paste_checkbox.into()
    ]).align_y(iced::alignment::Vertical::Center);

    // Save & Cancel Buttons
    let save_btn = button(text("Save").size(12).color(Color::WHITE))
        .on_press(Message::SaveSettings)
        .padding([5, 14])
        .style(|_theme, status| {
            let bg = match status {
                iced::widget::button::Status::Hovered => Color::from_rgb(0.25, 0.55, 0.9),
                iced::widget::button::Status::Pressed => Color::from_rgb(0.15, 0.45, 0.8),
                _ => Color::from_rgb(0.2, 0.5, 0.85),
            };
            button::Style {
                background: Some(bg.into()),
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 4.0.into(),
                },
                shadow: iced::Shadow::default(),
                text_color: Color::WHITE,
            }
        });

    let cancel_btn = button(text("Cancel").size(12).color(Color::from_rgb(0.75, 0.75, 0.75)))
        .on_press(Message::CancelSettings)
        .padding([5, 14])
        .style(|_theme, status| {
            let bg = match status {
                iced::widget::button::Status::Hovered => Color::from_rgba(1.0, 1.0, 1.0, 0.08),
                iced::widget::button::Status::Pressed => Color::from_rgba(1.0, 1.0, 1.0, 0.04),
                _ => Color::from_rgba(1.0, 1.0, 1.0, 0.01),
            };
            button::Style {
                background: Some(bg.into()),
                border: Border {
                    color: Color::from_rgba(1.0, 1.0, 1.0, 0.15),
                    width: 1.0,
                    radius: 4.0.into(),
                },
                shadow: iced::Shadow::default(),
                text_color: Color::from_rgb(0.75, 0.75, 0.75),
            }
        });

    let buttons_row = row(vec![cancel_btn.into(), save_btn.into()])
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center);

    let actions_row = row(vec![
        iced::widget::horizontal_space().into(),
        buttons_row.into()
    ])
    .width(Length::Fill);

    let fields_col = column(vec![
        model_row.into(),
        lang_row.into(),
        hotkey_row.into(),
        paste_row.into()
    ])
    .spacing(8)
    .width(Length::Fill);

    let main_col = column(vec![
        header.into(),
        fields_col.into(),
        actions_row.into()
    ])
    .spacing(12)
    .width(Length::Fill);

    let panel = container(main_col)
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme| container::Style {
            background: Some(Color::from_rgba(0.05, 0.05, 0.07, 0.94).into()),
            text_color: Some(Color::from_rgb(0.9, 0.9, 0.9)),
            border: Border {
                color: Color::from_rgba(0.35, 0.65, 1.0, 0.28),
                width: 1.5,
                radius: 12.0.into(),
            },
            shadow: iced::Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.4),
                offset: iced::Vector::new(0.0, 4.0),
                blur_radius: 16.0,
            },
        });

    mouse_area(panel)
        .on_press(Message::DragWindow)
        .into()
}

/// Membaca isi direktori "models/" secara sinkron dan menggabungkannya dengan daftar model Whisper standar.
fn list_available_models() -> Vec<String> {
    let mut models = vec![
        "ggml-tiny.bin".to_string(),
        "ggml-base.bin".to_string(),
        "ggml-small.bin".to_string(),
        "ggml-medium.bin".to_string(),
    ];

    // Pindai folder models/ untuk mendeteksi file model kustom (.bin) lainnya
    if let Ok(entries) = std::fs::read_dir("models") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map_or(false, |ext| ext == "bin") {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    let name_str = name.to_string();
                    if !models.contains(&name_str) {
                        models.push(name_str);
                    }
                }
            }
        }
    }

    models.sort();
    models
}
