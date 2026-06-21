//! View helpers untuk floating pill (background, waveform, caret).
//!
//! Dipakai dari `app::view()`. Setiap helper punya signature generik terhadap
//! `Message` agar bisa dipakai ulang di view apa pun yang menurunkan dari
//! `iced::Renderer` (default-nya `iced::Renderer` = `tiny_skia` di desktop).

use std::collections::VecDeque;

use iced::mouse;
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Program};
use iced::widget::container::{self, Style};
use iced::widget::{row, text};
use iced::{Border, Color, Element, Length, Point, Rectangle, Renderer, Theme};

/// Panjang rolling history untuk waveform bars (40 sample).
pub const AMP_HISTORY_LEN: usize = 40;

/// Période blink caret dalam frame (60fps): 30 frame = 0.5 detik on/off.
pub const CARET_BLINK_PERIOD_FRAMES: u64 = 30;

/// Lebar pill (canvas waveform).
pub const WAVEFORM_WIDTH: f32 = 120.0;
/// Tinggi pill (canvas waveform).
pub const WAVEFORM_HEIGHT: f32 = 32.0;

/// Background pill: semi-transparent dark dengan rounded 999, border bercahaya, dan bayangan lembut.
pub fn pill_background() -> Style {
    Style {
        background: Some(Color::from_rgba(0.04, 0.04, 0.06, 0.92).into()),
        text_color: Some(Color::from_rgba(0.95, 0.96, 0.98, 1.0)),
        border: Border {
            color: Color::from_rgba(0.35, 0.65, 1.0, 0.25),
            width: 1.5,
            radius: 999.0.into(),
        },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.45),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 16.0,
        },
        ..Style::default()
    }
}

/// Program canvas untuk waveform bars (40 bar vertikal).
pub struct Waveform {
    /// Snapshot amplitude history (0.0 - 1.0 per sample).
    pub history: Vec<f32>,
}

impl<Message> Program<Message> for Waveform {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        _renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(_renderer, bounds.size());
        let w = bounds.width;
        let h = bounds.height;
        if w <= 0.0 || h <= 0.0 {
            return vec![frame.into_geometry()];
        }

        // Padding internal 4px biar bar tidak mepet edge.
        let pad = 4.0;
        let inner_w = w - 2.0 * pad;
        let inner_h = h - 2.0 * pad;
        if inner_w <= 0.0 || inner_h <= 0.0 {
            return vec![frame.into_geometry()];
        }

        // Lebar per bar = inner_w / AMP_HISTORY_LEN; gap 1px.
        let slot = inner_w / AMP_HISTORY_LEN as f32;
        let bar_w = (slot - 1.0).max(1.0);

        let baseline_y = bounds.y + pad + inner_h * 0.5;

        for i in 0..AMP_HISTORY_LEN {
            // Ambil amplitude untuk bar index i; kalau history lebih pendek,
            // pakai 0.0 (idle bar).
            let amp = self.history.get(i).copied().unwrap_or(0.0);
            // Clamp 0.0 - 1.0 agar tidak out-of-bounds visual.
            let amp = amp.clamp(0.0, 1.0);

            // Tinggi minimal 2px biar tetap kelihatan.
            let bar_h = (amp * inner_h * 0.5).max(2.0);
            let x_center = bounds.x + pad + i as f32 * slot + 0.5 + bar_w * 0.5;
            let y_top = baseline_y - bar_h;
            let y_bottom = baseline_y + bar_h;

            // Gradient: transisi dari cyan ke indigo/violet yang halus.
            let t = i as f32 / (AMP_HISTORY_LEN - 1).max(1) as f32;
            let r = 0.30 + 0.40 * t; // 0.30 ke 0.70
            let g = 0.70 - 0.30 * t; // 0.70 ke 0.40
            let b = 1.00;
            let color_active = Color::from_rgba(r, g, b, 0.4 + 0.6 * amp);
            let color_idle = Color::from_rgba(1.0, 1.0, 1.0, 0.15);

            // Warna: lebih pekat kalau amplitude tinggi.
            let fill_color = if amp > 0.02 {
                color_active
            } else {
                color_idle
            };

            let path = Path::new(|builder| {
                builder.move_to(Point::new(x_center, y_top));
                builder.line_to(Point::new(x_center, y_bottom));
            });

            let stroke = canvas::Stroke {
                style: canvas::Style::Solid(fill_color),
                width: bar_w,
                line_cap: canvas::LineCap::Round,
                ..Default::default()
            };
            frame.stroke(&path, stroke);
        }

        vec![frame.into_geometry()]
    }
}

/// Build widget canvas waveform dari `VecDeque` amplitude.
pub fn waveform_canvas<'a, Message: 'a>(
    history: &VecDeque<f32>,
) -> Canvas<Waveform, Message> {
    // Snapshot ke Vec<f32> supaya 'static-friendly untuk Program.
    let snapshot: Vec<f32> = history.iter().copied().collect();
    canvas::Canvas::new(Waveform { history: snapshot })
        .width(Length::Fixed(WAVEFORM_WIDTH))
        .height(Length::Fixed(WAVEFORM_HEIGHT))
}

/// Caret blink: batang cyan bercahaya 1.5x18 yang on/off berdasarkan frame counter.
pub fn blinking_caret<'a, Message: 'a>(frame: u64) -> Element<'a, Message> {
    let visible = (frame / CARET_BLINK_PERIOD_FRAMES) % 2 == 0;
    let color = if visible {
        Color::from_rgba(0.40, 0.85, 1.00, 0.95)
    } else {
        Color::TRANSPARENT
    };

    // Batang vertikal 1.5 x 18, dibungkus dalam container kecil.
    container::Container::new(
        container::Container::new(iced::widget::Space::new(
            Length::Fixed(1.5),
            Length::Fixed(18.0),
        ))
        .width(Length::Fixed(1.5))
        .height(Length::Fixed(18.0))
        .style(move |_theme| Style {
            background: Some(color.into()),
            ..Style::default()
        }),
    )
    .width(Length::Fixed(1.5))
    .height(Length::Fixed(18.0))
    .into()
}

/// Pulsing dot: indikator rekaman merah yang berdenyut lembut (pulsing) dengan glow ring.
pub fn pulsing_dot<'a, Message: 'a>(frame: u64) -> Element<'a, Message> {
    struct DotProgram {
        frame: u64,
    }

    impl<Message> canvas::Program<Message> for DotProgram {
        type State = ();

        fn draw(
            &self,
            _state: &Self::State,
            _renderer: &Renderer,
            _theme: &Theme,
            bounds: Rectangle,
            _cursor: mouse::Cursor,
        ) -> Vec<Geometry> {
            let mut frame = Frame::new(_renderer, bounds.size());
            let center = Point::new(bounds.width * 0.5, bounds.height * 0.5);
            let radius = 3.5;

            // Pulse logic: sinus wave untuk opacity
            let t = self.frame as f32 * 0.08;
            let alpha = 0.4 + 0.6 * (t.sin() * 0.5 + 0.5);

            let circle = Path::circle(center, radius);
            frame.fill(&circle, Color::from_rgba(1.0, 0.3, 0.3, alpha));

            // Efek glow ring luar
            let glow_radius = radius + 2.0 + (t.sin() * 0.5 + 0.5) * 2.0;
            let glow_circle = Path::circle(center, glow_radius);
            frame.fill(&glow_circle, Color::from_rgba(1.0, 0.3, 0.3, alpha * 0.25));

            vec![frame.into_geometry()]
        }
    }

    canvas::Canvas::new(DotProgram { frame })
        .width(Length::Fixed(16.0))
        .height(Length::Fixed(16.0))
        .into()
}


/// Spinner 3 titik yang bergerak berdasarkan frame counter.
pub fn spinner_dots<'a, Message: 'a>(frame: u64) -> Element<'a, Message> {
    let phase = (frame / 10) % 4;
    let dot_str = match phase {
        0 => "·  ",
        1 => "·· ",
        2 => "···",
        _ => "   ",
    };
    text(dot_str)
        .size(16)
        .color(Color::from_rgba(0.35, 0.65, 1.0, 1.0))
        .into()
}

/// Re-export row builder untuk pill content.
pub fn pill_row<'a, Message>(children: Vec<Element<'a, Message>>) -> Element<'a, Message>
where
    Message: 'a + Clone,
{
    row(children)
        .spacing(10)
        .align_y(iced::alignment::Vertical::Center)
        .into()
}

/// Menggambar logo mic SVG menggunakan canvas path.
pub fn mic_icon<'a, Message: 'a>(color: Color, size: f32) -> Element<'a, Message> {
    struct MicProgram {
        color: Color,
    }

    impl<Message> canvas::Program<Message> for MicProgram {
        type State = ();

        fn draw(
            &self,
            _state: &Self::State,
            _renderer: &Renderer,
            _theme: &Theme,
            bounds: Rectangle,
            _cursor: mouse::Cursor,
        ) -> Vec<Geometry> {
            let mut frame = Frame::new(_renderer, bounds.size());
            let cx = bounds.width * 0.5;
            let cy = bounds.height * 0.5;

            // Skala relatif terhadap ukuran container (diasumsikan basis 24px)
            let scale = bounds.width / 24.0; 

            // 1. Mic capsule (batang utama rounded di tengah)
            let cap_w = 5.0 * scale;
            let cap_h = 10.0 * scale;
            let cap_y = cy - cap_h * 0.5 - 2.0 * scale;
            
            let cap_path = Path::new(|builder| {
                builder.move_to(Point::new(cx, cap_y + 2.5 * scale));
                builder.line_to(Point::new(cx, cap_y + cap_h - 2.5 * scale));
            });
            let cap_stroke = canvas::Stroke {
                style: canvas::Style::Solid(self.color),
                width: cap_w,
                line_cap: canvas::LineCap::Round,
                ..Default::default()
            };
            frame.stroke(&cap_path, cap_stroke);

            // 2. U-shape cup (cangkir penahan mic)
            let cup_stroke = canvas::Stroke {
                style: canvas::Style::Solid(self.color),
                width: 1.5 * scale,
                line_cap: canvas::LineCap::Round,
                ..Default::default()
            };

            let cup_path = Path::new(|builder| {
                builder.move_to(Point::new(cx - 5.5 * scale, cy - 2.0 * scale));
                builder.line_to(Point::new(cx - 5.5 * scale, cy + 1.0 * scale));
                builder.quadratic_curve_to(
                    Point::new(cx - 5.5 * scale, cy + 4.0 * scale),
                    Point::new(cx, cy + 4.0 * scale),
                );
                builder.quadratic_curve_to(
                    Point::new(cx + 5.5 * scale, cy + 4.0 * scale),
                    Point::new(cx + 5.5 * scale, cy + 1.0 * scale),
                );
                builder.line_to(Point::new(cx + 5.5 * scale, cy - 2.0 * scale));
            });
            frame.stroke(&cup_path, cup_stroke);

            // 3. Stand/Leher mic (batang vertikal penyangga)
            let stem_path = Path::new(|builder| {
                builder.move_to(Point::new(cx, cy + 4.0 * scale));
                builder.line_to(Point::new(cx, cy + 8.0 * scale));
            });
            frame.stroke(&stem_path, cup_stroke);

            // 4. Base mic (garis horizontal penyangga bawah)
            let base_path = Path::new(|builder| {
                builder.move_to(Point::new(cx - 4.0 * scale, cy + 8.0 * scale));
                builder.line_to(Point::new(cx + 4.0 * scale, cy + 8.0 * scale));
            });
            frame.stroke(&base_path, cup_stroke);

            vec![frame.into_geometry()]
        }
    }

    canvas::Canvas::new(MicProgram { color })
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}
