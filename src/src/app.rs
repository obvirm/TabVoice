use std::collections::VecDeque;
use std::sync::{mpsc::Receiver, Arc};

use eframe::egui;

use crate::events::{AppEvent, TrayAction};
use crate::state::AppState;

pub const AMP_HISTORY_LEN: usize = 40;
pub const WAVEFORM_BARS: usize = 3;

pub static IS_ASSIGNING_HOTKEY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Debug, Clone, PartialEq)]
pub enum UiMode {
    Idle,
    Recording,
    Processing,
    Done,
    Settings,
}

pub struct TabVoice {
    pub mode: UiMode,
    pub text: String,
    pub amp_history: VecDeque<f32>,
    pub frame: u64,
    pub fade_timer: u32,
    pub available_models: Vec<String>,
    pub available_microphones: Vec<String>,
    
    // Settings state
    pub temp_model_path: String,
    pub temp_language: String,
    pub temp_hotkey: String,
    pub temp_paste: bool,
    pub temp_device_name: String,
    pub temp_dark_mode: bool,
    pub temp_realtime: bool,
    pub temp_auto_start: bool,
    pub temp_vad_threshold: f32,
    
    pub current_window_size: egui::Vec2,
    pub download_progress: Option<f32>,
    pub assigning_hotkey: bool,

    // Shared
    pub state: Arc<AppState>,
    pub event_rx: Receiver<AppEvent>,
    pub event_tx: crate::events::EventSender,
}

pub struct AppFlags {
    pub event_rx: Receiver<AppEvent>,
    pub event_tx: crate::events::EventSender,
    pub state: Arc<AppState>,
}

impl TabVoice {
    pub fn new(flags: AppFlags) -> Self {
        Self {
            mode: UiMode::Idle,
            text: String::new(),
            amp_history: VecDeque::with_capacity(AMP_HISTORY_LEN),
            frame: 0,
            fade_timer: 0,
            available_models: Vec::new(),
            available_microphones: Vec::new(),
            temp_model_path: String::new(),
            temp_language: String::new(),
            temp_hotkey: String::new(),
            temp_paste: true,
            temp_device_name: String::new(),
            temp_dark_mode: true,
            temp_realtime: false,
            temp_auto_start: false,
            temp_vad_threshold: 0.005,
            current_window_size: egui::vec2(48.0, 48.0),
            download_progress: None,
            assigning_hotkey: false,
            state: flags.state,
            event_rx: flags.event_rx,
            event_tx: flags.event_tx,
        }
    }

    fn apply_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Amplitude { value } => {
                let amp = value.clamp(0.0, 1.0);
                if self.amp_history.len() >= AMP_HISTORY_LEN {
                    self.amp_history.pop_front();
                }
                self.amp_history.push_back(amp);

                if self.mode != UiMode::Recording && self.mode != UiMode::Settings {
                    self.mode = UiMode::Recording;
                    self.text.clear();
                    self.fade_timer = 0;
                }
            }
            AppEvent::TranscriptionPartial { text } => {
                self.text = text;
            }
            AppEvent::Done { text } => {
                self.text = text;
                self.mode = UiMode::Done;
                self.fade_timer = 90; // 1.5s @ 60fps
                self.amp_history.clear();
            }
            AppEvent::Error { message } => {
                self.text = format!("Error: {}", message);
                self.mode = UiMode::Done;
                self.fade_timer = 120; // 2s @ 60fps
                self.amp_history.clear();
            }
            AppEvent::ActiveHotkeyCaptured => {
                let current_settings = self.state.settings.lock().unwrap().clone();
                self.temp_hotkey = current_settings.hotkey;
                self.assigning_hotkey = false;
                IS_ASSIGNING_HOTKEY.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            AppEvent::HotkeyPressed | AppEvent::HotkeyReleased => {}
            AppEvent::TrayAction(action) => self.handle_tray(action),
            AppEvent::DownloadProgress { progress } => {
                self.download_progress = Some(progress);
            }
            AppEvent::DownloadComplete => {
                self.download_progress = None;
                // Reload model ke memory
                let current_model_path = self.state.settings.lock().unwrap().model_path.clone();
                if current_model_path.exists() {
                    match crate::ffi::WhisperModel::from_file(&current_model_path) {
                        Ok(new_model) => {
                            let mut m = self.state.model.lock().unwrap();
                            *m = Some(std::sync::Arc::new(new_model));
                            self.text = "Model berhasil dimuat!".to_string();
                            self.mode = UiMode::Done;
                            self.fade_timer = 120;
                        }
                        Err(e) => {
                            self.text = format!("Gagal memuat model: {}", e);
                            self.mode = UiMode::Done;
                            self.fade_timer = 120;
                        }
                    }
                }
            }
        }
    }

    fn handle_tray(&mut self, action: TrayAction) {
        match action {
            TrayAction::ReloadModel => {
                let current_model_path = self.state.settings.lock().unwrap().model_path.clone();
                if current_model_path.exists() {
                    let _ = self.event_tx.send(AppEvent::DownloadComplete);
                } else {
                    let model_name = current_model_path.file_name().unwrap().to_str().unwrap().to_string();
                    let url = format!(
                        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
                        model_name
                    );
                    self.download_progress = Some(0.0);
                    let tx = self.event_tx.clone();
                    
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        rt.block_on(async move {
                            if let Ok(mut resp) = reqwest::get(&url).await {
                                let total = resp.content_length().unwrap_or(0) as f32;
                                let mut downloaded = 0.0;
                                
                                if let Some(parent) = current_model_path.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                
                                if let Ok(mut file) = std::fs::File::create(&current_model_path) {
                                    while let Ok(Some(chunk)) = resp.chunk().await {
                                        use std::io::Write;
                                        let _ = file.write_all(&chunk);
                                        downloaded += chunk.len() as f32;
                                        if total > 0.0 {
                                            let _ = tx.send(AppEvent::DownloadProgress { progress: downloaded / total });
                                        }
                                    }
                                }
                            }
                            let _ = tx.send(AppEvent::DownloadComplete);
                        });
                    });
                }
            }
            TrayAction::OpenSettings => {
                self.mode = UiMode::Settings;
                self.available_models = list_available_models();
                self.available_microphones = crate::audio::get_available_microphones();
                let current = self.state.settings.lock().unwrap().clone();
                
                let model_filename = current.model_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("ggml-base.bin")
                    .to_string();
                self.temp_model_path = model_filename;
                
                self.temp_language = current.language.unwrap_or_else(|| "auto".to_string());
                self.temp_hotkey = current.hotkey.clone();
                self.temp_paste = current.paste_on_release;
                self.temp_device_name = current.device_name.unwrap_or_else(|| "System Default".to_string());
                self.temp_dark_mode = current.dark_mode;
                self.temp_realtime = current.realtime;
                self.temp_auto_start = current.auto_start;
                self.temp_vad_threshold = current.vad_threshold;
            }
            TrayAction::Quit => {
                std::process::exit(0);
            }
        }
    }
}

impl eframe::App for TabVoice {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // Transparent background
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let is_dark = self.state.settings.lock().unwrap().dark_mode;
        
        // Set egui global visuals
        if is_dark {
            ctx.set_visuals(egui::Visuals::dark());
        } else {
            ctx.set_visuals(egui::Visuals::light());
        }

        let prev_mode = self.mode.clone();

        // Increment frame counter for animations
        self.frame = self.frame.wrapping_add(1);

        // Process all pending events
        while let Ok(ev) = self.event_rx.try_recv() {
            self.apply_event(ev);
        }

        // Handle fade timer
        if self.fade_timer > 0 {
            self.fade_timer = self.fade_timer.saturating_sub(1);
            if self.fade_timer == 0 && self.mode == UiMode::Done {
                self.mode = UiMode::Idle;
                self.text.clear();
                self.amp_history.clear();
            }
        }

        let target_size = match self.mode {
            UiMode::Settings => egui::vec2(480.0, 420.0),
            UiMode::Idle => egui::vec2(52.0, 52.0),
            UiMode::Recording => {
                if self.text.is_empty() { egui::vec2(144.0, 52.0) } else { egui::vec2(284.0, 52.0) }
            },
            UiMode::Processing => {
                if self.text.is_empty() { egui::vec2(144.0, 52.0) } else { egui::vec2(284.0, 52.0) }
            },
            UiMode::Done => egui::vec2(284.0, 52.0),
        };

        if self.current_window_size != target_size {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(target_size));
            self.current_window_size = target_size;
        }

        // Draw UI
        let panel_frame = egui::Frame::none()
            .fill(egui::Color32::TRANSPARENT)
            .inner_margin(0.0);

        egui::CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
            if self.mode == UiMode::Settings {
                self.show_settings(ctx, ui, frame);
            } else {
                self.show_pill(ctx, ui, frame);
            }
        });

        if prev_mode != self.mode {
            let next_target = match self.mode {
                UiMode::Settings => egui::vec2(480.0, 420.0),
                UiMode::Idle => egui::vec2(52.0, 52.0),
                UiMode::Recording => {
                    if self.text.is_empty() { egui::vec2(144.0, 52.0) } else { egui::vec2(284.0, 52.0) }
                },
                UiMode::Processing => {
                    if self.text.is_empty() { egui::vec2(144.0, 52.0) } else { egui::vec2(284.0, 52.0) }
                },
                UiMode::Done => egui::vec2(284.0, 52.0),
            };
            if self.current_window_size != next_target {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(next_target));
                self.current_window_size = next_target;
            }
        }

        // Selalu request repaint agar animasi jalan dan polling event lancar (menggantikan fungsi Tick di iced)
        ctx.request_repaint();
    }
}

impl TabVoice {
    fn show_pill(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, _eframe: &mut eframe::Frame) {
        // Container utama pill, di-shrink sedikit agar outline (stroke) tidak terpotong tepi window
        let pill_rect = egui::Rect::from_center_size(
            ui.max_rect().center(),
            egui::vec2(ui.max_rect().width().min(284.0), ui.max_rect().height().min(52.0))
        ).shrink(2.0);

        let mut ui = ui.child_ui(pill_rect, egui::Layout::centered_and_justified(egui::Direction::LeftToRight));

        let is_dark = self.state.settings.lock().unwrap().dark_mode;
        
        let (bg_color, _stroke_color) = if is_dark {
            (
                egui::Color32::from_rgba_unmultiplied(30, 30, 30, 235),
                egui::Color32::from_rgba_unmultiplied(100, 150, 255, 217)
            )
        } else {
            (
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 235),
                egui::Color32::from_rgba_unmultiplied(64, 115, 217, 217)
            )
        };

        // Animasi outline kuning kedip-kedip pelan (pulse)
        let time = ui.input(|i| i.time);
        // (time * 2.0).sin() memberikan gelombang dari -1.0 ke 1.0. (Satu siklus sekitar 3 detik)
        let pulse = (time * 2.0).sin() * 0.5 + 0.5;
        // Rentang alpha dari 40 hingga 180 agar kedipan halus dan tidak menyilaukan
        let outline_alpha = (40.0 + 140.0 * pulse) as u8;
        let outline_color = egui::Color32::from_rgba_unmultiplied(255, 215, 0, outline_alpha); // Warna kuning emas

        let pill_frame = egui::Frame::none()
            .fill(bg_color)
            .rounding(24.0)
            .stroke(egui::Stroke::new(2.0, outline_color))
            .inner_margin(egui::Margin::symmetric(14.0, 8.0));

        let response = pill_frame.show(&mut ui, |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;

                // Drag window with pill
                let interact = ui.interact(ui.max_rect(), ui.id().with("drag"), egui::Sense::drag());
                if interact.dragged() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                let (idle_mic, rec_mic, text_color, text_err) = if is_dark {
                    (
                        egui::Color32::from_rgb(180, 190, 210), // lighter gray-blue
                        egui::Color32::from_rgb(100, 170, 255), // light blue
                        egui::Color32::from_rgb(230, 230, 240),
                        egui::Color32::from_rgb(255, 100, 100),
                    )
                } else {
                    (
                        egui::Color32::from_rgb(76, 89, 115),
                        egui::Color32::from_rgb(51, 115, 242),
                        egui::Color32::from_rgb(25, 30, 46),
                        egui::Color32::from_rgb(199, 46, 46),
                    )
                };

                // Inner content
                match self.mode {
                    UiMode::Idle => {
                        self.draw_mic(ui, idle_mic);
                    }
                    UiMode::Recording => {
                        self.draw_mic(ui, rec_mic);
                        self.draw_waveform(ui, is_dark);
                        if !self.text.is_empty() {
                            let display = if self.text.chars().count() > 40 {
                                let truncated: String = self.text.chars().take(40).collect();
                                format!("{}…", truncated)
                            } else {
                                self.text.clone()
                            };
                            ui.add(egui::Label::new(
                                egui::RichText::new(display).color(text_color).size(13.0)
                            ).truncate(true));
                        }
                    }
                    UiMode::Processing => {
                        self.draw_mic(ui, rec_mic);
                        self.draw_spinner(ui, rec_mic);
                        if !self.text.is_empty() {
                            let display = if self.text.chars().count() > 40 {
                                let truncated: String = self.text.chars().take(40).collect();
                                format!("{}…", truncated)
                            } else {
                                self.text.clone()
                            };
                            ui.add(egui::Label::new(
                                egui::RichText::new(display).color(text_color).size(13.0)
                            ).truncate(true));
                        }
                    }
                    UiMode::Done => {
                        let color = if self.text.starts_with("Error:") { text_err } else { text_color };
                        let display = if self.text.chars().count() > 60 {
                            let truncated: String = self.text.chars().take(60).collect();
                            format!("{}…", truncated)
                        } else {
                            self.text.clone()
                        };
                        ui.add(egui::Label::new(
                            egui::RichText::new(display).color(color).size(13.0)
                        ).truncate(true));
                    }
                    UiMode::Settings => {}
                }
            })
        });

        // Kalau mic area di klik, toggle recording
        let mut interact_rect = response.response.rect;
        // Hanya area kiri (mic) yang clickable biar ga konflik sama drag
        interact_rect.set_width(40.0);
        let click_sense = ui.interact(interact_rect, ui.id().with("mic_btn"), egui::Sense::click());
        if click_sense.clicked() {
            if self.mode == UiMode::Recording {
                crate::hotkey::handle_release(&self.event_tx, &self.state);
            } else {
                crate::hotkey::handle_press(&self.event_tx, &self.state);
            }
        }
    }

    fn draw_mic(&self, ui: &mut egui::Ui, color: egui::Color32) {
        let (rect, _response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
        let cx = rect.center().x;
        let cy = rect.center().y;
        let scale = 18.0 / 24.0;
        let stroke = egui::Stroke::new(1.5 * scale, color);

        if let Some(progress) = self.download_progress {
            let r = 12.0;
            let mut arc_path = vec![];
            let steps = 32;
            let start_angle = -std::f32::consts::PI / 2.0;
            let end_angle = start_angle + progress * 2.0 * std::f32::consts::PI;
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let angle = start_angle + t * (end_angle - start_angle);
                arc_path.push(egui::pos2(cx + r * angle.cos(), cy + r * angle.sin()));
            }
            ui.painter().add(egui::Shape::line(arc_path, egui::Stroke::new(2.5, egui::Color32::from_rgb(0, 255, 150))));
        }

        let cap_w = 5.0 * scale;
        let cap_h = 10.0 * scale;
        let cap_rect = egui::Rect::from_center_size(
            egui::pos2(cx, cy - 2.0 * scale),
            egui::vec2(cap_w, cap_h)
        );
        ui.painter().rect_stroke(cap_rect, cap_w * 0.5, egui::Stroke::new(1.5 * scale, color));

        let mut cup_path = vec![
            egui::pos2(cx - 5.5 * scale, cy - 2.0 * scale),
            egui::pos2(cx - 5.5 * scale, cy + 1.0 * scale),
        ];
        // simple cup approximation
        cup_path.push(egui::pos2(cx - 4.0 * scale, cy + 3.5 * scale));
        cup_path.push(egui::pos2(cx, cy + 4.5 * scale));
        cup_path.push(egui::pos2(cx + 4.0 * scale, cy + 3.5 * scale));
        cup_path.push(egui::pos2(cx + 5.5 * scale, cy + 1.0 * scale));
        cup_path.push(egui::pos2(cx + 5.5 * scale, cy - 2.0 * scale));
        ui.painter().add(egui::Shape::line(cup_path, stroke));

        ui.painter().line_segment([egui::pos2(cx, cy + 4.5 * scale), egui::pos2(cx, cy + 8.0 * scale)], stroke);
        ui.painter().line_segment([egui::pos2(cx - 4.0 * scale, cy + 8.0 * scale), egui::pos2(cx + 4.0 * scale, cy + 8.0 * scale)], stroke);
    }

    fn draw_waveform(&self, ui: &mut egui::Ui, is_dark: bool) {
        let (rect, _response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
        
        // Ambil amplitude terbaru
        let latest_amp = self.amp_history.back().copied().unwrap_or(0.0);
        
        // Boost sensitivity and smooth it
        let amp = (latest_amp * 25.0).clamp(0.0, 1.0).sqrt();
        
        let pad = 2.0;
        let inner_w = 14.0;
        let inner_h = 14.0;
        let slot = inner_w / WAVEFORM_BARS as f32;
        let bar_w = 2.2;
        let baseline_y = rect.min.y + pad + inner_h * 0.5;

        for i in 0..WAVEFORM_BARS {
            // Berikan variasi tinggi untuk tiap bar menggunakan fungsi sinus berdasarkan frame, 
            // tapi amplitudonya tetap dikontrol oleh seberapa keras suara (amp).
            let phase = (self.frame as f32 * 0.25) + (i as f32 * 2.0);
            let wave_mult = 0.5 + 0.5 * phase.sin(); // berosilasi antara 0.0 - 1.0
            
            // Base height 2.5px agar bar tetap terlihat saat diam.
            // Saat bersuara, tingginya ditambah animasi osilasi.
            let active_h = amp * wave_mult * (inner_h - 2.5);
            let bar_h = 2.5 + active_h;
            
            // Alpha berdenyut sedikit seiring dengan volume
            let alpha = (0.4 + 0.6 * amp) * 255.0;
            let color = if is_dark {
                egui::Color32::from_rgba_unmultiplied(100, 170, 255, alpha as u8)
            } else {
                egui::Color32::from_rgba_unmultiplied(51, 115, 242, alpha as u8)
            };
            
            let x_center = rect.min.x + pad + i as f32 * slot + slot * 0.5;
            ui.painter().line_segment(
                [egui::pos2(x_center, baseline_y - bar_h * 0.5), egui::pos2(x_center, baseline_y + bar_h * 0.5)],
                egui::Stroke::new(bar_w, color)
            );
        }
    }

    fn draw_spinner(&self, ui: &mut egui::Ui, color: egui::Color32) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
        let phase = (self.frame / 10) % 4;
        let dot_str = match phase {
            0 => "·  ",
            1 => "·· ",
            2 => "···",
            _ => "   ",
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            dot_str,
            egui::FontId::proportional(16.0),
            color,
        );
    }

    fn show_settings(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, _eframe: &mut eframe::Frame) {
        let panel_rect = ui.max_rect().shrink(16.0);
        let mut ui = ui.child_ui(panel_rect, egui::Layout::top_down(egui::Align::Center));
        
        let interact = ui.interact(ui.max_rect(), ui.id().with("drag_settings"), egui::Sense::drag());
        if interact.dragged() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        let is_dark = self.state.settings.lock().unwrap().dark_mode;
        
        let (bg_color, _stroke_color, text_color, header_color) = if is_dark {
            (
                egui::Color32::from_rgba_unmultiplied(40, 40, 40, 245),
                egui::Color32::from_rgba_unmultiplied(100, 150, 255, 140),
                egui::Color32::from_rgb(200, 210, 220),
                egui::Color32::from_rgb(120, 170, 255)
            )
        } else {
            (
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 245),
                egui::Color32::from_rgba_unmultiplied(51, 89, 217, 140),
                egui::Color32::from_rgb(64, 76, 102),
                egui::Color32::from_rgb(51, 89, 217)
            )
        };

        let frame = egui::Frame::none()
            .fill(bg_color)
            .rounding(12.0)
            .stroke(egui::Stroke::NONE)
            .inner_margin(16.0);

        frame.show(&mut ui, |ui| {
            ui.label(egui::RichText::new("TabVoice Settings").size(15.0).color(header_color));
            ui.add_space(12.0);

            egui::Grid::new("settings_grid")
                .num_columns(2)
                .spacing([16.0, 12.0])
                .min_col_width(100.0)
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Microphone").color(text_color));
                    egui::ComboBox::from_id_source("mic_combo")
                        .width(180.0)
                        .selected_text(&self.temp_device_name)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.temp_device_name, "System Default".to_string(), "System Default");
                            for m in &self.available_microphones {
                                ui.selectable_value(&mut self.temp_device_name, m.clone(), m);
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("Model").color(text_color));
                    egui::ComboBox::from_id_source("model_combo")
                        .width(180.0)
                        .selected_text(&self.temp_model_path)
                        .show_ui(ui, |ui| {
                            for m in &self.available_models {
                                ui.selectable_value(&mut self.temp_model_path, m.clone(), m);
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("Language Hint").color(text_color));
                    
                    static SUPPORTED_LANGUAGES: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
                    let lang_options = SUPPORTED_LANGUAGES.get_or_init(|| crate::ffi::get_supported_languages());
                    
                    let current_lang_name = lang_options.iter()
                        .find(|(code, _)| *code == self.temp_language)
                        .map(|(_, name)| name.as_str())
                        .unwrap_or("Auto Detect");
                        
                    egui::ComboBox::from_id_source("lang_combo")
                        .width(180.0)
                        .selected_text(current_lang_name)
                        .show_ui(ui, |ui| {
                            for (code, name) in lang_options {
                                ui.selectable_value(&mut self.temp_language, code.to_string(), name);
                            }
                        });
                    ui.end_row();

                    ui.label(egui::RichText::new("Hotkey").color(text_color));
                    if self.assigning_hotkey {
                        let btn = ui.button("Listening... Press keys!");
                        if btn.clicked_elsewhere() {
                            self.assigning_hotkey = false;
                            IS_ASSIGNING_HOTKEY.store(false, std::sync::atomic::Ordering::Relaxed);
                        } else {
                            ctx.input(|i| {
                                let mut mods = Vec::new();
                                if i.modifiers.ctrl { mods.push("Ctrl"); }
                                if i.modifiers.shift { mods.push("Shift"); }
                                if i.modifiers.alt { mods.push("Alt"); }
                                
                                let mut primary_key = None;
                                for key in &i.keys_down {
                                    primary_key = Some(format!("{:?}", key));
                                }
                                
                                if let Some(pk) = primary_key {
                                    mods.push(&pk);
                                    self.temp_hotkey = mods.join("+");
                                    self.assigning_hotkey = false;
                                    IS_ASSIGNING_HOTKEY.store(false, std::sync::atomic::Ordering::Relaxed);
                                }
                            });
                        }
                    } else {
                        if ui.button(&self.temp_hotkey).clicked() {
                            self.assigning_hotkey = true;
                            IS_ASSIGNING_HOTKEY.store(true, std::sync::atomic::Ordering::Relaxed);
                            self.temp_hotkey.clear();
                        }
                    }
                    ui.end_row();

                    ui.label(egui::RichText::new("Theme").color(text_color));
                    ui.checkbox(&mut self.temp_dark_mode, "Dark Mode");
                    ui.end_row();

                    ui.label(egui::RichText::new("Real-Time").color(text_color));
                    ui.checkbox(&mut self.temp_realtime, "Live transcription (beta)");
                    ui.end_row();

                    ui.label("Anti-Hallucination:");
                    ui.add(egui::Slider::new(&mut self.temp_vad_threshold, 0.0..=0.05)
                        .text("Silence cutoff (VAD)")
                        .step_by(0.001));
                    ui.end_row();

                    ui.label("");
                    ui.checkbox(&mut self.temp_paste, "Auto-paste on release");
                    ui.end_row();

                    ui.label("");
                    ui.checkbox(&mut self.temp_auto_start, "Run at Startup (Auto-start)");
                    ui.end_row();
                });

            ui.add_space(16.0);
            
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Save").clicked() {
                        let new_settings = crate::settings::Settings {
                            model_path: crate::settings::get_models_dir().join(&self.temp_model_path),
                            language: if self.temp_language == "auto" || self.temp_language.trim().is_empty() { None } else { Some(self.temp_language.trim().to_string()) },
                            hotkey: self.temp_hotkey.clone(),
                            paste_on_release: self.temp_paste,
                            device_name: if self.temp_device_name == "System Default" { None } else { Some(self.temp_device_name.clone()) },
                            dark_mode: self.temp_dark_mode,
                            realtime: self.temp_realtime,
                            auto_start: self.temp_auto_start,
                            vad_threshold: self.temp_vad_threshold,
                        };

                        if let Err(e) = crate::settings::save(&new_settings) {
                            log::error!("Gagal menyimpan settings: {e}");
                        }
                        *self.state.settings.lock().unwrap() = new_settings;
                        self.mode = UiMode::Idle;
                    }
                    if ui.button("Cancel").clicked() {
                        self.mode = UiMode::Idle;
                    }
                });
            });
        });
    }
}

fn list_available_models() -> Vec<String> {
    let mut models = vec![
        "ggml-tiny.en.bin".to_string(),
        "ggml-tiny.bin".to_string(),
        "ggml-base.en.bin".to_string(),
        "ggml-base.bin".to_string(),
        "ggml-small.en.bin".to_string(),
        "ggml-small.bin".to_string(),
        "ggml-medium.en.bin".to_string(),
        "ggml-medium.bin".to_string(),
        "ggml-large-v1.bin".to_string(),
        "ggml-large-v2.bin".to_string(),
        "ggml-large-v3.bin".to_string(),
        "ggml-large-v3-turbo.bin".to_string(),
        "ggml-large-v3-turbo-q5_0.bin".to_string(),
    ];

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
