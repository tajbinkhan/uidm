//! The toolbar under the menu bar: big buttons with a label under the icon,
//! or small icon-only ones (View > Toolbar). The buttons give the same
//! commands as the menus, and are grayed out when there is nothing for them
//! to act on. The icons are drawn here from plain shapes.

use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, Vec2};

use crate::i18n::tr;
use crate::menubar::{Command, State};

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Add,
    Resume,
    Stop,
    StopAll,
    Delete,
    DeleteDone,
    Options,
    Scheduler,
    StartQueue,
    StopQueue,
    Grabber,
}

struct Button {
    icon: Icon,
    label: &'static str,
    tip: &'static str,
    command: Command,
    enabled: bool,
    /// For the queue buttons: a drop-down with each queue, and whether the
    /// entry is for starting (or stopping) it.
    queues: Option<bool>,
}

/// Big buttons are this size, as in the toolbar UIDM's layout follows.
const LARGE: Vec2 = Vec2::new(58.0, 50.0);
const SMALL: Vec2 = Vec2::new(28.0, 26.0);

/// Draw the toolbar. Returns the command of a button clicked.
pub fn show(ui: &mut egui::Ui, s: &State, small: bool) -> Option<Command> {
    let on = s.connected;
    let set = s.settings.is_some() && on;
    let t = &s.ticked;
    let buttons = [
        Button {
            icon: Icon::Add,
            label: tr("Add URL"),
            tip: tr("Add a download by its address"),
            command: Command::AddUrl,
            enabled: on,
            queues: None,
        },
        Button {
            icon: Icon::Resume,
            label: tr("Resume"),
            tip: tr("Start or resume the selected downloads"),
            command: Command::StartTicked,
            enabled: on && t.startable,
            queues: None,
        },
        Button {
            icon: Icon::Stop,
            label: tr("Stop"),
            tip: tr("Stop the selected downloads"),
            command: Command::StopTicked,
            enabled: on && t.running,
            queues: None,
        },
        Button {
            icon: Icon::StopAll,
            label: tr("Stop All"),
            tip: tr("Stop every download"),
            command: Command::PauseAll,
            enabled: on,
            queues: None,
        },
        Button {
            icon: Icon::Delete,
            label: tr("Delete"),
            tip: tr("Remove the selected downloads from the list"),
            command: Command::RemoveTicked,
            enabled: on && t.any,
            queues: None,
        },
        Button {
            icon: Icon::DeleteDone,
            label: tr("Delete Completed"),
            tip: tr("Remove every finished download from the list"),
            command: Command::DeleteCompleted,
            enabled: on,
            queues: None,
        },
        Button {
            icon: Icon::Options,
            label: tr("Options"),
            tip: tr("Browser capture, file types, connection, proxy, logins and more"),
            command: Command::Options,
            enabled: set,
            queues: None,
        },
        Button {
            icon: Icon::Scheduler,
            label: tr("Scheduler"),
            tip: tr("Queues and their schedules"),
            command: Command::Scheduler,
            enabled: on,
            queues: None,
        },
        Button {
            icon: Icon::StartQueue,
            label: tr("Start Queue"),
            tip: tr("Start the main queue, or pick a queue from the arrow"),
            command: Command::StartQueue(uidm::queue::MAIN_QUEUE),
            enabled: on,
            queues: Some(true),
        },
        Button {
            icon: Icon::StopQueue,
            label: tr("Stop Queue"),
            tip: tr("Stop the main queue, or pick a queue from the arrow"),
            command: Command::StopQueue(uidm::queue::MAIN_QUEUE),
            enabled: on,
            queues: Some(false),
        },
        Button {
            icon: Icon::Grabber,
            label: tr("Grabber"),
            tip: tr("Find and download the files a site links to"),
            command: Command::Grabber,
            enabled: on,
            queues: None,
        },
    ];

    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, b) in buttons.iter().enumerate() {
            // Gaps where the groups change, as in the menus.
            if [1, 4, 6, 8, 10].contains(&i) {
                ui.add_space(4.0);
            }
            if button(ui, b, small).clicked() {
                chosen = Some(b.command.clone());
            }
            if let Some(start) = b.queues {
                let arrow = ui.add_enabled_ui(b.enabled, |ui| {
                    // A slim strip beside the button, framed only on hover.
                    let style = ui.style_mut();
                    style.spacing.button_padding = Vec2::new(1.0, 0.0);
                    style.spacing.interact_size.y = if small { SMALL.y } else { LARGE.y };
                    style.visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                    style.visuals.widgets.inactive.bg_stroke = Stroke::NONE;
                    ui.menu_button(egui::RichText::new("⏷").size(10.0), |ui| {
                        for (id, name, running) in &s.queues {
                            if ui.add_enabled(*running != start, egui::Button::new(name)).clicked() {
                                chosen = Some(if start { Command::StartQueue(*id) } else { Command::StopQueue(*id) });
                                ui.close_menu();
                            }
                        }
                    })
                });
                arrow.response.on_hover_text(b.tip);
            }
        }
    });
    chosen
}

fn button(ui: &mut egui::Ui, b: &Button, small: bool) -> egui::Response {
    let size = if small { SMALL } else { LARGE };
    let sense = if b.enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let response = response.on_hover_text(if small { format!("{}: {}", b.label, b.tip) } else { b.tip.to_string() });
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let visuals = ui.visuals();
    if b.enabled && (response.hovered() || response.has_focus()) {
        let fill = if response.is_pointer_button_down_on() {
            visuals.widgets.active.weak_bg_fill
        } else {
            visuals.widgets.hovered.weak_bg_fill
        };
        ui.painter().rect_filled(rect, 3.0, fill);
    }
    let color = if b.enabled {
        tint(b.icon, visuals.dark_mode)
    } else {
        visuals.widgets.noninteractive.fg_stroke.color.gamma_multiply(0.45)
    };
    let icon_rect = if small {
        Rect::from_center_size(rect.center(), Vec2::splat(18.0))
    } else {
        Rect::from_center_size(Pos2::new(rect.center().x, rect.top() + 16.0), Vec2::splat(24.0))
    };
    draw_icon(ui.painter(), b.icon, icon_rect, color);
    if !small {
        let text_color = if b.enabled {
            visuals.text_color()
        } else {
            visuals.weak_text_color().gamma_multiply(0.7)
        };
        // One line: a label too long for the button ends in an ellipsis.
        let mut job = egui::text::LayoutJob::simple_singleline(b.label.to_string(), egui::FontId::proportional(11.5), text_color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 2.0);
        let galley = ui.painter().layout_job(job);
        let pos = Pos2::new(rect.center().x - galley.size().x / 2.0, rect.bottom() - galley.size().y - 2.0);
        ui.painter().galley(pos, galley, text_color);
    }
    response
}

/// Each button's color, so the row reads at a glance.
fn tint(icon: Icon, dark: bool) -> Color32 {
    let (light, deep) = match icon {
        Icon::Add => (Color32::from_rgb(0x4a, 0xc2, 0x7d), Color32::from_rgb(0x1f, 0x8a, 0x4c)),
        Icon::Resume | Icon::StartQueue => (Color32::from_rgb(0x5b, 0xa8, 0xf5), Color32::from_rgb(0x1f, 0x6f, 0xc4)),
        Icon::Stop | Icon::StopAll | Icon::StopQueue => (Color32::from_rgb(0xf0, 0x8c, 0x5a), Color32::from_rgb(0xc2, 0x55, 0x1f)),
        Icon::Delete | Icon::DeleteDone => (Color32::from_rgb(0xe8, 0x6a, 0x7a), Color32::from_rgb(0xb8, 0x2e, 0x44)),
        Icon::Options | Icon::Scheduler => (Color32::from_rgb(0xa9, 0x9a, 0xf0), Color32::from_rgb(0x5b, 0x48, 0xc2)),
        Icon::Grabber => (Color32::from_rgb(0xe0, 0xb8, 0x4a), Color32::from_rgb(0x9a, 0x74, 0x0c)),
    };
    if dark {
        light
    } else {
        deep
    }
}

fn draw_icon(p: &egui::Painter, icon: Icon, r: Rect, color: Color32) {
    let w = r.width();
    let stroke = Stroke::new((w / 12.0).max(1.5), color);
    let c = r.center();
    let at = |x: f32, y: f32| Pos2::new(r.left() + x * w, r.top() + y * w);
    match icon {
        Icon::Add => {
            p.circle_stroke(c, w * 0.45, stroke);
            p.line_segment([at(0.5, 0.27), at(0.5, 0.73)], stroke);
            p.line_segment([at(0.27, 0.5), at(0.73, 0.5)], stroke);
        }
        Icon::Resume => {
            p.circle_stroke(c, w * 0.45, stroke);
            play(p, Rect::from_center_size(at(0.53, 0.5), Vec2::splat(w * 0.4)), color);
        }
        Icon::Stop => {
            p.circle_stroke(c, w * 0.45, stroke);
            p.rect_filled(Rect::from_center_size(c, Vec2::splat(w * 0.34)), 1.5, color);
        }
        Icon::StopAll => {
            p.rect_stroke(Rect::from_min_size(at(0.08, 0.08), Vec2::splat(w * 0.6)), 2.0, stroke, egui::StrokeKind::Middle);
            p.rect_filled(Rect::from_min_size(at(0.34, 0.34), Vec2::splat(w * 0.58)), 2.0, color);
        }
        Icon::Delete | Icon::DeleteDone => {
            // A bin: lid, handle and body.
            p.line_segment([at(0.14, 0.24), at(0.86, 0.24)], stroke);
            p.line_segment([at(0.4, 0.12), at(0.6, 0.12)], stroke);
            let body = [at(0.22, 0.24), at(0.28, 0.92), at(0.72, 0.92), at(0.78, 0.24)];
            p.add(Shape::line(body.to_vec(), stroke));
            if icon == Icon::Delete {
                p.line_segment([at(0.42, 0.4), at(0.42, 0.78)], stroke);
                p.line_segment([at(0.58, 0.4), at(0.58, 0.78)], stroke);
            } else {
                p.add(Shape::line(vec![at(0.35, 0.58), at(0.47, 0.72), at(0.67, 0.44)], stroke));
            }
        }
        Icon::Options => {
            // A cog: eight teeth around a ring.
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let dir = Vec2::angled(a);
                p.line_segment([c + dir * w * 0.3, c + dir * w * 0.47], Stroke::new(w * 0.14, color));
            }
            p.circle_stroke(c, w * 0.3, stroke);
            p.circle_stroke(c, w * 0.11, stroke);
        }
        Icon::Scheduler => {
            p.circle_stroke(c, w * 0.45, stroke);
            p.line_segment([c, at(0.5, 0.22)], stroke);
            p.line_segment([c, at(0.7, 0.62)], stroke);
        }
        Icon::StartQueue | Icon::StopQueue => {
            // A list, with play or stop over its corner.
            for y in [0.18, 0.38, 0.58] {
                p.line_segment([at(0.06, y), at(0.6, y)], stroke);
            }
            let badge = Rect::from_center_size(at(0.7, 0.7), Vec2::splat(w * 0.4));
            if icon == Icon::StartQueue {
                play(p, badge, color);
            } else {
                p.rect_filled(badge.shrink(w * 0.03), 1.5, color);
            }
        }
        Icon::Grabber => {
            // A globe with a down arrow.
            p.circle_stroke(at(0.42, 0.42), w * 0.36, stroke);
            p.line_segment([at(0.06, 0.42), at(0.78, 0.42)], stroke);
            p.add(Shape::line(
                vec![at(0.42, 0.06), at(0.28, 0.24), at(0.28, 0.6), at(0.42, 0.78)],
                stroke,
            ));
            p.add(Shape::line(
                vec![at(0.42, 0.06), at(0.56, 0.24), at(0.56, 0.6), at(0.42, 0.78)],
                stroke,
            ));
            let arrow = Stroke::new(stroke.width * 1.2, color);
            p.line_segment([at(0.82, 0.56), at(0.82, 0.96)], arrow);
            p.add(Shape::line(vec![at(0.68, 0.82), at(0.82, 0.96), at(0.96, 0.82)], arrow));
        }
    }
}

fn play(p: &egui::Painter, r: Rect, color: Color32) {
    let pts = vec![
        Pos2::new(r.left() + r.width() * 0.15, r.top()),
        Pos2::new(r.right(), r.center().y),
        Pos2::new(r.left() + r.width() * 0.15, r.bottom()),
    ];
    p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}
