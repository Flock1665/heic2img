use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crate::{
    collect_files, convert_files_parallel, default_threads, logical_threads, BatchEvent,
    ConvertOptions, Format,
};

struct HeicApp {
    inputs: Vec<PathBuf>,
    format: Format,
    quality: u8,
    out_dir: String,
    recursive: bool,
    replace_original: bool,
    threads: usize,
    max_threads: usize,
    running: bool,
    done: usize,
    total: usize,
    batch_start: Option<Instant>,
    finished: Option<String>,
    /// 按文件序号有序存放的日志行（并行完成顺序不定，按序显示）
    slots: Vec<Option<String>>,
    summary: Option<String>,
    rx: Option<Receiver<BatchEvent>>,
}

impl Default for HeicApp {
    fn default() -> Self {
        let max_threads = logical_threads();
        let threads = default_threads().clamp(1, max_threads);
        Self {
            inputs: Vec::new(),
            format: Format::Jpg,
            quality: 92,
            out_dir: String::new(),
            recursive: true,
            replace_original: false,
            threads,
            max_threads,
            running: false,
            done: 0,
            total: 0,
            batch_start: None,
            finished: None,
            slots: Vec::new(),
            summary: None,
            rx: None,
        }
    }
}

fn load_cjk_font(ctx: &egui::Context) {
    let candidates = [
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/simsun.ttc",
    ];
    let mut fonts = egui::FontDefinitions::default();
    // 未启用 egui default_fonts（省 1.4MB 内嵌字体），系统字体是唯一来源
    let mut loaded = false;
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".into(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".into());
            }
            loaded = true;
            break;
        }
    }
    assert!(loaded, "未找到可用系统字体（微软雅黑/黑体/宋体），界面无法渲染");
    ctx.set_fonts(fonts);
}

impl HeicApp {
    fn drain_events(&mut self) {
        loop {
            let ev = {
                let Some(rx) = &self.rx else { return };
                match rx.try_recv() {
                    Ok(ev) => ev,
                    Err(_) => return,
                }
            };
            match ev {
                BatchEvent::Started { total } => {
                    self.total = total;
                    self.done = 0;
                    self.slots = vec![None; total];
                    self.batch_start = Some(Instant::now());
                }
                BatchEvent::ItemDone { index, done, line } => {
                    if index < self.slots.len() {
                        self.slots[index] = Some(line);
                    }
                    self.done = done;
                }
                BatchEvent::Finished {
                    ok,
                    failed,
                    elapsed_ms,
                } => {
                    self.running = false;
                    let secs = elapsed_ms as f64 / 1000.0;
                    self.summary = Some(if ok == 0 && failed == 0 {
                        "未找到任何 HEIC/HEIF 文件".to_string()
                    } else {
                        let rate = if secs > 0.0 {
                            self.total as f64 / secs
                        } else {
                            0.0
                        };
                        if failed > 0 {
                            format!("完成：成功 {ok} 个，失败 {failed} 个（{secs:.1}s，{rate:.1} 文件/秒）")
                        } else {
                            format!("全部完成：成功 {ok} 个（{secs:.1}s，{rate:.1} 文件/秒）")
                        }
                    });
                    self.finished = self.summary.clone();
                    self.batch_start = None;
                    self.rx = None;
                }
            }
        }
    }

    fn add_input(&mut self, path: PathBuf) {
        if !self.inputs.contains(&path) {
            self.inputs.push(path);
        }
    }

    fn start(&mut self) {
        if self.inputs.is_empty() {
            self.summary = Some("请先添加 HEIC/HEIF 文件或目录".into());
            return;
        }
        let trimmed = self.out_dir.trim().to_string();
        let out_root = if trimmed.is_empty() {
            None
        } else {
            Some(PathBuf::from(trimmed))
        };
        if let Some(root) = &out_root {
            if let Err(e) = fs::create_dir_all(root) {
                self.summary = Some(format!("错误: 无法创建输出目录 {}: {e}", root.display()));
                return;
            }
        }
        let opts = ConvertOptions {
            format: self.format,
            quality: self.quality,
            out_root,
            replace_original: self.replace_original,
        };
        let inputs = self.inputs.clone();
        let recursive = self.recursive;
        let threads = self.threads;

        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.running = true;
        self.done = 0;
        self.total = 0;
        self.batch_start = None;
        self.finished = None;
        self.summary = None;
        self.slots.clear();

        std::thread::spawn(move || {
            let files = collect_files(&inputs, recursive);
            convert_files_parallel(files, &opts, threads, tx);
        });
    }
}

impl eframe::App for HeicApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        if self.running {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("HEIC 批量转换");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("清空").clicked() {
                        self.inputs.clear();
                    }
                    if ui.button("添加目录…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.add_input(dir);
                        }
                    }
                    if ui.button("添加文件…").clicked() {
                        if let Some(paths) = rfd::FileDialog::new()
                            .add_filter("HEIC 图片", &["heic", "heif"])
                            .pick_files()
                        {
                            for p in paths {
                                self.add_input(p);
                            }
                        }
                    }
                });
            });
            ui.add_space(4.0);

            ui.label("输入列表：");
            egui::ScrollArea::vertical()
                .id_salt("inputs")
                .max_height(140.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if self.inputs.is_empty() {
                        ui.weak("（空）点击右上角按钮添加文件或目录");
                    }
                    let mut remove: Option<usize> = None;
                    for (i, p) in self.inputs.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let tag = if p.is_dir() { "[目录]" } else { "[文件]" };
                            ui.monospace(format!("{tag} {}", p.display()));
                            if ui.small_button("×").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        self.inputs.remove(i);
                    }
                });
            ui.add_space(6.0);

            egui::Grid::new("options")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    ui.label("输出格式:");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.format, Format::Jpg, "JPG");
                        ui.radio_value(&mut self.format, Format::Png, "PNG");
                        ui.radio_value(&mut self.format, Format::Both, "JPG + PNG");
                    });
                    ui.end_row();

                    ui.label("JPEG 质量:");
                    ui.add(egui::Slider::new(&mut self.quality, 1..=100).text("质量"));
                    ui.end_row();

                    ui.label("并行线程:");
                    ui.add_enabled(
                        !self.running,
                        egui::Slider::new(&mut self.threads, 1..=self.max_threads)
                            .text("线程（默认 = 物理核数，最大 = 逻辑核数）"),
                    );
                    ui.end_row();

                    ui.label("输出目录:");
                    ui.horizontal(|ui| {
                        let edit = egui::TextEdit::singleline(&mut self.out_dir)
                            .hint_text("（留空 = 与源文件同目录）")
                            .desired_width(ui.available_width() - 70.0);
                        ui.add(edit);
                        if ui.button("浏览…").clicked() {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.out_dir = dir.display().to_string();
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("选项:");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.recursive, "递归子目录");
                        ui.checkbox(&mut self.replace_original, "覆盖原文件（转换后删除源 HEIC）");
                    });
                    ui.end_row();
                });
            if self.replace_original {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("⚠ 已开启覆盖原文件：同名输出将被直接覆盖，转换成功后源 HEIC/HEIF 文件将被删除（不可恢复）")
                        .small()
                        .color(egui::Color32::from_rgb(200, 70, 70)),
                );
            }
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let label = if self.running {
                    format!("转换中… {}/{}", self.done, self.total)
                } else {
                    "开始转换".to_string()
                };
                let btn = egui::Button::new(label).min_size(egui::vec2(120.0, 0.0));
                if ui.add_enabled(!self.running, btn).clicked() {
                    self.start();
                }
                if let Some(msg) = &self.finished {
                    ui.label(msg);
                }
            });
            if self.total > 0 {
                let frac = self.done as f32 / self.total as f32;
                let mut bar = egui::ProgressBar::new(frac)
                    .show_percentage()
                    .text(format!("{} / {}", self.done, self.total));
                if self.running {
                    if let Some(start) = self.batch_start {
                        let elapsed = start.elapsed().as_secs_f64();
                        if elapsed > 0.0 && self.done > 0 {
                            bar = bar.text(format!(
                                "{} / {}（{:.1} 文件/秒）",
                                self.done,
                                self.total,
                                self.done as f64 / elapsed
                            ));
                        }
                    }
                }
                ui.add(bar);
            }
            ui.add_space(6.0);

            ui.label("日志:");
            egui::ScrollArea::vertical()
                .id_salt("logs")
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if self.slots.iter().all(|s| s.is_none()) && self.summary.is_none() {
                        ui.weak("（暂无）");
                    }
                    for line in self.slots.iter().flatten() {
                        ui.monospace(line);
                    }
                    if let Some(s) = &self.summary {
                        ui.monospace(egui::RichText::new(s).strong());
                    }
                });
        });
    }
}

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([700.0, 640.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_title("heic2img — HEIC 批量转换"),
        ..Default::default()
    };
    eframe::run_native(
        "heic2img",
        options,
        Box::new(|cc| {
            load_cjk_font(&cc.egui_ctx);
            Ok(Box::new(HeicApp::default()))
        }),
    )
}
