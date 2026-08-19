pub mod gui;

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use heif_oxide::{DecodedImage, Pixels};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};
use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Jpg,
    Png,
    Both,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Jpg => "jpg",
            Format::Png => "png",
            Format::Both => "jpg + png",
        }
    }
    pub fn want_jpg(self) -> bool {
        matches!(self, Format::Jpg | Format::Both)
    }
    pub fn want_png(self) -> bool {
        matches!(self, Format::Png | Format::Both)
    }
}

#[derive(Clone)]
pub struct ConvertOptions {
    pub format: Format,
    pub quality: u8,
    /// 输出目录（None = 与源文件同目录）
    pub out_root: Option<PathBuf>,
    /// 覆盖原文件：同名输出直接覆盖（不加序号），且转换成功后删除源 HEIC/HEIF
    pub replace_original: bool,
}

pub struct ConvertResult {
    pub width: u32,
    pub height: u32,
    pub outputs: Vec<PathBuf>,
    pub source_removed: bool,
}

pub fn is_heic(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "heic" | "heif"))
        .unwrap_or(false)
}

fn collect_dir(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = entries.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for entry in items {
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                collect_dir(&path, recursive, out);
            }
        } else if is_heic(&path) {
            out.push(path);
        }
    }
}

pub fn collect_files(inputs: &[PathBuf], recursive: bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for p in inputs {
        if p.is_dir() {
            collect_dir(p, recursive, &mut files);
        } else if p.is_file() && is_heic(p) {
            files.push(p.clone());
        }
    }
    files
}

fn unique_path(out_dir: &Path, stem: &str, ext: &str, overwrite: bool) -> PathBuf {
    let primary = out_dir.join(format!("{stem}.{ext}"));
    if overwrite || !primary.exists() {
        return primary;
    }
    for i in 1.. {
        let candidate = out_dir.join(format!("{stem}_{i}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

struct ConvertTargets {
    jpg: Option<PathBuf>,
    png: Option<PathBuf>,
}

fn build_targets(
    src: &Path,
    format: Format,
    out_root: Option<&Path>,
    overwrite: bool,
) -> ConvertTargets {
    let out_dir = match out_root {
        Some(dir) => dir.to_path_buf(),
        None => src
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".")),
    };
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
    let jpg = format
        .want_jpg()
        .then(|| unique_path(&out_dir, stem, "jpg", overwrite));
    let png = format
        .want_png()
        .then(|| unique_path(&out_dir, stem, "png", overwrite));
    ConvertTargets { jpg, png }
}

/// 解码像素的零拷贝借用：8-bit 源直接借用内部缓冲区，
/// 10/12-bit 源才做一次 8-bit 降位转换。
enum PixelsRef<'a> {
    Rgb(&'a [u8]),
    Rgba(&'a [u8]),
    Converted(Vec<u8>),
}

impl PixelsRef<'_> {
    fn as_slice(&self) -> &[u8] {
        match self {
            PixelsRef::Rgb(v) => v,
            PixelsRef::Rgba(v) => v,
            PixelsRef::Converted(v) => v,
        }
    }
    fn has_alpha(&self) -> bool {
        !matches!(self, PixelsRef::Rgb(_))
    }
}

fn pixels_ref(decoded: &DecodedImage) -> PixelsRef<'_> {
    match &decoded.pixels {
        Pixels::Rgb8(v) => PixelsRef::Rgb(v),
        Pixels::Rgba8(v) => PixelsRef::Rgba(v),
        _ => PixelsRef::Converted(decoded.to_rgba8()),
    }
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
        }
    }
    Ok(())
}

/// jpeg-encoder（SIMD/AVX2）：RGBA 直接输入，alpha 自动忽略，无需 RGBA→RGB 转换。
fn write_jpeg(path: &Path, w: u32, h: u32, px: &PixelsRef, quality: u8) -> Result<()> {
    let file = fs::File::create(path).with_context(|| path.display().to_string())?;
    let mut writer = BufWriter::new(file);
    let data = px.as_slice();
    let ctype = if px.has_alpha() {
        JpegColorType::Rgba
    } else {
        JpegColorType::Rgb
    };
    let w = u16::try_from(w).context("图像宽度超过 JPEG 上限 65535")?;
    let h = u16::try_from(h).context("图像高度超过 JPEG 上限 65535")?;
    JpegEncoder::new(&mut writer, quality)
        .encode(data, w, h, ctype)
        .map_err(|e| anyhow::anyhow!("JPEG 编码失败: {e}"))
        .with_context(|| path.display().to_string())?;
    writer.flush().with_context(|| path.display().to_string())?;
    Ok(())
}

/// PNG 快速档：zlib Fast 压缩 + 自适应滤波，RGB 源不扩展 alpha（省 25% 数据量）。
fn write_png(path: &Path, w: u32, h: u32, px: &PixelsRef) -> Result<()> {
    let file = fs::File::create(path).with_context(|| path.display().to_string())?;
    let mut writer = BufWriter::new(file);
    let data = px.as_slice();
    let color = if px.has_alpha() {
        ExtendedColorType::Rgba8
    } else {
        ExtendedColorType::Rgb8
    };
    PngEncoder::new_with_quality(&mut writer, CompressionType::Fast, FilterType::Adaptive)
        .write_image(data, w, h, color)
        .with_context(|| path.display().to_string())?;
    writer.flush().with_context(|| path.display().to_string())?;
    Ok(())
}

fn convert_one(src: &Path, targets: &ConvertTargets, quality: u8) -> Result<(u32, u32)> {
    let decoded = heif_oxide::decode_file(src.to_string_lossy().as_ref())
        .map_err(|e| anyhow::anyhow!("解码失败: {e}"))
        .with_context(|| src.display().to_string())?;
    let (w, h) = (decoded.width, decoded.height);
    let px = pixels_ref(&decoded);

    if let Some(jpg_path) = &targets.jpg {
        ensure_parent(jpg_path)?;
        write_jpeg(jpg_path, w, h, &px, quality)?;
    }
    if let Some(png_path) = &targets.png {
        ensure_parent(png_path)?;
        write_png(png_path, w, h, &px)?;
    }
    Ok((w, h))
}

pub fn convert_file(src: &Path, opts: &ConvertOptions) -> Result<ConvertResult> {
    let targets = build_targets(
        src,
        opts.format,
        opts.out_root.as_deref(),
        opts.replace_original,
    );
    let (width, height) = convert_one(src, &targets, opts.quality)?;
    let mut outputs = Vec::new();
    if let Some(p) = &targets.jpg {
        outputs.push(p.clone());
    }
    if let Some(p) = &targets.png {
        outputs.push(p.clone());
    }
    let source_removed = if opts.replace_original {
        fs::remove_file(src).with_context(|| format!("删除源文件失败: {}", src.display()))?;
        true
    } else {
        false
    };
    Ok(ConvertResult {
        width,
        height,
        outputs,
        source_removed,
    })
}

pub fn logical_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// 默认并行度 = 物理核数。解码/编码为标量整数密集型，
/// 超线程无收益（实测 6C/12T：6 线程 ~41 文件/秒 vs 12 线程 ~36 且波动大）。
pub fn default_threads() -> usize {
    let logical = logical_threads();
    physical_cores().map_or(logical, |p| p.clamp(1, logical))
}

#[cfg(windows)]
fn physical_cores() -> Option<usize> {
    use windows_sys::Win32::System::SystemInformation::{
        GetLogicalProcessorInformation, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION,
    };
    let mut len = 0u32;
    unsafe {
        if GetLogicalProcessorInformation(std::ptr::null_mut(), &mut len) != 0 {
            return None;
        }
    }
    let count = len as usize / std::mem::size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION>();
    if count == 0 {
        return None;
    }
    let mut buf: Vec<SYSTEM_LOGICAL_PROCESSOR_INFORMATION> =
        (0..count).map(|_| Default::default()).collect();
    if unsafe { GetLogicalProcessorInformation(buf.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    let cores = buf
        .iter()
        .filter(|i| i.Relationship == RelationProcessorCore)
        .count();
    (cores > 0).then_some(cores)
}

#[cfg(not(windows))]
fn physical_cores() -> Option<usize> {
    None
}

pub enum BatchEvent {
    Started { total: usize },
    ItemDone { index: usize, done: usize, line: String },
    Finished { ok: usize, failed: usize, elapsed_ms: u128 },
}

fn item_line(index: usize, total: usize, src: &Path, res: &ConvertResult) -> String {
    let names: Vec<String> = res
        .outputs
        .iter()
        .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
        .collect();
    let repl = if res.source_removed {
        "（源文件已删除）"
    } else {
        ""
    };
    format!(
        "[{}/{}] {} -> {} ({}x{}) ✓{}",
        index + 1,
        total,
        src.display(),
        names.join(" + "),
        res.width,
        res.height,
        repl
    )
}

/// 并行批量转换：N 个工作线程通过原子索引领取文件，逐文件回报进度事件。
/// heif-oxide 无全局状态，decode_file 可安全并发调用。
pub fn convert_files_parallel(
    files: Vec<PathBuf>,
    opts: &ConvertOptions,
    threads: usize,
    tx: Sender<BatchEvent>,
) {
    let total = files.len();
    let _ = tx.send(BatchEvent::Started { total });
    if total == 0 {
        let _ = tx.send(BatchEvent::Finished {
            ok: 0,
            failed: 0,
            elapsed_ms: 0,
        });
        return;
    }
    let start = Instant::now();
    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let ok = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let files = Arc::new(files);
    let opts = Arc::new(opts.clone());
    let threads = threads.clamp(1, total);

    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let next = Arc::clone(&next);
        let done = Arc::clone(&done);
        let ok = Arc::clone(&ok);
        let failed = Arc::clone(&failed);
        let files = Arc::clone(&files);
        let opts = Arc::clone(&opts);
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= files.len() {
                break;
            }
            let src = files[i].clone();
            let line = match convert_file(&src, &opts) {
                Ok(res) => {
                    ok.fetch_add(1, Ordering::Relaxed);
                    item_line(i, total, &src, &res)
                }
                Err(e) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    format!("[{}/{}] {} ✗ {:#}", i + 1, total, src.display(), e)
                }
            };
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = tx.send(BatchEvent::ItemDone {
                index: i,
                done: d,
                line,
            });
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    let _ = tx.send(BatchEvent::Finished {
        ok: ok.load(Ordering::Relaxed),
        failed: failed.load(Ordering::Relaxed),
        elapsed_ms: start.elapsed().as_millis(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn is_heic_by_extension() {
        assert!(is_heic(Path::new("a.HEIC")));
        assert!(is_heic(Path::new("b.heif")));
        assert!(!is_heic(Path::new("c.jpg")));
        assert!(!is_heic(Path::new("noext")));
    }

    #[test]
    fn default_threads_within_logical_bounds() {
        let logical = logical_threads();
        assert!(logical >= 1);
        let d = default_threads();
        assert!((1..=logical).contains(&d), "默认线程 {d} 超出 [1, {logical}]");
        #[cfg(windows)]
        if let Some(p) = physical_cores() {
            assert_eq!(d, p.min(logical));
        }
    }

    #[test]
    fn unique_path_appends_suffix() {
        let dir = std::env::temp_dir();
        let stem = format!("heic2img_test_{}", std::process::id());
        let first = unique_path(&dir, &stem, "jpg", false);
        fs::write(&first, b"x").unwrap();
        let second = unique_path(&dir, &stem, "jpg", false);
        assert_ne!(first, second);
        let forced = unique_path(&dir, &stem, "jpg", true);
        assert_eq!(forced, first);
        let _ = fs::remove_file(&first);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("heic2img_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn replace_original_removes_source() {
        let sample = Path::new("testdata/C006.heic");
        if !sample.exists() {
            return;
        }
        let dir = temp_dir("repl");
        let src = dir.join("样本.heic");
        fs::copy(sample, &src).unwrap();
        let opts = ConvertOptions {
            format: Format::Jpg,
            quality: 90,
            out_root: None,
            replace_original: true,
        };
        let res = convert_file(&src, &opts).unwrap();
        assert!(res.source_removed);
        assert!(!src.exists(), "源文件应被删除");
        assert_eq!(res.outputs, vec![dir.join("样本.jpg")]);
        assert!(dir.join("样本.jpg").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_source_and_suffixes_without_replace() {
        let sample = Path::new("testdata/C006.heic");
        if !sample.exists() {
            return;
        }
        let dir = temp_dir("keep");
        let src = dir.join("a.heic");
        fs::copy(sample, &src).unwrap();
        let opts = ConvertOptions {
            format: Format::Jpg,
            quality: 90,
            out_root: None,
            replace_original: false,
        };
        let first = convert_file(&src, &opts).unwrap();
        assert!(!first.source_removed);
        assert!(src.exists(), "源文件应保留");
        assert_eq!(first.outputs, vec![dir.join("a.jpg")]);
        let second = convert_file(&src, &opts).unwrap();
        assert_eq!(second.outputs, vec![dir.join("a_1.jpg")], "不覆盖时应自动加序号");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parallel_batch_smoke() {
        let sample = Path::new("testdata/C006.heic");
        if !sample.exists() {
            return;
        }
        let base = temp_dir("par");
        let src_dir = base.join("s");
        fs::create_dir_all(&src_dir).unwrap();
        for i in 0..4 {
            fs::copy(sample, src_dir.join(format!("p{i}.heic"))).unwrap();
        }
        let opts = ConvertOptions {
            format: Format::Both,
            quality: 90,
            out_root: Some(base.join("o")),
            replace_original: false,
        };
        let files = collect_files(&[src_dir], false);
        assert_eq!(files.len(), 4);
        let (tx, rx) = mpsc::channel();
        convert_files_parallel(files, &opts, 3, tx);
        let mut ok = 0;
        let mut items = 0;
        let mut started_total = 0;
        for ev in rx.try_iter() {
            match ev {
                BatchEvent::Started { total } => started_total = total,
                BatchEvent::ItemDone { .. } => items += 1,
                BatchEvent::Finished { ok: o, .. } => ok = o,
            }
        }
        assert_eq!(started_total, 4);
        assert_eq!(items, 4);
        assert_eq!(ok, 4);
        for i in 0..4 {
            assert!(base.join("o").join(format!("p{i}.jpg")).exists());
            assert!(base.join("o").join(format!("p{i}.png")).exists());
        }
        let _ = fs::remove_dir_all(&base);
    }

    /// 分相基准：解码 / 完整 JPG / 完整 PNG。
    /// 运行: cargo test --release bench_phases -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_phases() {
        let sample = Path::new("testdata/C006.heic");
        if !sample.exists() {
            eprintln!("skip: no testdata/C006.heic");
            return;
        }
        let n = 24;
        let dir = temp_dir("ph");
        let src = dir.join("x.heic");
        fs::copy(sample, &src).unwrap();

        let t = Instant::now();
        for _ in 0..n {
            let d = heif_oxide::decode_file(src.to_string_lossy().as_ref()).unwrap();
            let _ = d.channels();
        }
        let decode_s = t.elapsed().as_secs_f64() / n as f64;

        let bench = |format: Format, tag: &str| {
            let out = dir.join(tag);
            fs::create_dir_all(&out).unwrap();
            let opts = ConvertOptions {
                format,
                quality: 92,
                out_root: Some(out),
                replace_original: false,
            };
            let t = Instant::now();
            for _ in 0..n {
                convert_file(&src, &opts).unwrap();
            }
            t.elapsed().as_secs_f64() / n as f64
        };
        let jpg_s = bench(Format::Jpg, "oj");
        let png_s = bench(Format::Png, "op");
        eprintln!(
            "decode: {decode_s:.3}s/file | jpg: {jpg_s:.3}s | png: {png_s:.3}s | jpg enc+io: {:.3}s | png enc+io: {:.3}s",
            jpg_s - decode_s,
            png_s - decode_s
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 输出有效性：JPG SOI/EOI 标记 + PNG 往返解码尺寸一致。
    #[test]
    fn outputs_are_valid_jpeg_and_png() {
        let sample = Path::new("testdata/C006.heic");
        if !sample.exists() {
            return;
        }
        let dir = temp_dir("valid");
        let src = dir.join("v.heic");
        fs::copy(sample, &src).unwrap();
        let decoded = heif_oxide::decode_file(src.to_string_lossy().as_ref()).unwrap();
        let (w, h) = (decoded.width, decoded.height);
        let opts = ConvertOptions {
            format: Format::Both,
            quality: 92,
            out_root: Some(dir.join("o")),
            replace_original: false,
        };
        let res = convert_file(&src, &opts).unwrap();
        assert_eq!((res.width, res.height), (w, h));

        let jpg = fs::read(dir.join("o").join("v.jpg")).unwrap();
        assert!(jpg.starts_with(&[0xFF, 0xD8, 0xFF]), "JPG 缺少 SOI 标记");
        assert!(jpg.ends_with(&[0xFF, 0xD9]), "JPG 缺少 EOI 标记");
        assert!(jpg.len() > 1024, "JPG 体积异常偏小");

        let png = image::ImageReader::open(dir.join("o").join("v.png"))
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!((png.width(), png.height()), (w, h));
        let _ = fs::remove_dir_all(&dir);
    }

    /// 诊断：C006 像素格式/尺寸 + 输出文件大小。
    /// 运行: cargo test --release debug_info -- --ignored --nocapture
    #[test]
    #[ignore]
    fn debug_info() {
        let sample = Path::new("testdata/C006.heic");
        if !sample.exists() {
            return;
        }
        let d = heif_oxide::decode_file(sample.to_string_lossy().as_ref()).unwrap();
        let variant = match d.pixels {
            Pixels::Rgb8(_) => "Rgb8",
            Pixels::Rgba8(_) => "Rgba8",
            Pixels::Rgb16(_) => "Rgb16",
            Pixels::Rgba16(_) => "Rgba16",
        };
        eprintln!(
            "C006: {}x{} {variant}, channels={}, src={}B",
            d.width,
            d.height,
            d.channels(),
            fs::metadata(sample).unwrap().len()
        );
        let dir = temp_dir("dbg");
        let opts = ConvertOptions {
            format: Format::Both,
            quality: 92,
            out_root: Some(dir.join("o")),
            replace_original: false,
        };
        convert_file(sample, &opts).unwrap();
        for f in fs::read_dir(dir.join("o")).unwrap().flatten() {
            eprintln!("  {} {}B", f.file_name().to_string_lossy(), f.metadata().unwrap().len());
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// 基准：48 文件 JPG 输出，完整线程曲线 1/2/3/4/6/8/12（第二轮热缓存复测 1/6/12）。
    /// 运行: cargo test --release bench_scaling -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_scaling() {
        let sample = Path::new("testdata/C006.heic");
        if !sample.exists() {
            eprintln!("skip: no testdata/C006.heic");
            return;
        }
        let n = 48;
        let base = std::env::temp_dir().join(format!("heic2img_bench_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let src_dir = base.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        for i in 0..n {
            fs::copy(sample, src_dir.join(format!("b{i:03}.heic"))).unwrap();
        }
        let files = collect_files(&[src_dir.clone()], false);
        assert_eq!(files.len(), n);

        let mut baseline = 0.0f64;
        for pass in 0..2 {
            let mut first = true;
            for &t in &[1usize, 2, 3, 4, 6, 8, 12] {
                if pass == 1 && !matches!(t, 1 | 6 | 12) {
                    continue;
                }
                let out_dir = base.join(format!("p{pass}t{t}"));
                fs::create_dir_all(&out_dir).unwrap();
                let opts = ConvertOptions {
                    format: Format::Jpg,
                    quality: 92,
                    out_root: Some(out_dir.clone()),
                    replace_original: false,
                };
                let (tx, rx) = mpsc::channel();
                let files_t = files.clone();
                let start = Instant::now();
                let h = std::thread::spawn(move || convert_files_parallel(files_t, &opts, t, tx));
                let mut events = 0;
                for ev in rx.iter() {
                    if matches!(ev, BatchEvent::Finished { .. }) {
                        events += 1;
                        break;
                    }
                    events += 1;
                }
                h.join().unwrap();
                let secs = start.elapsed().as_secs_f64();
                let outputs = fs::read_dir(&out_dir).unwrap().count();
                let rate = n as f64 / secs;
                if pass == 0 && first {
                    baseline = rate;
                    first = false;
                }
                let speedup = rate / baseline;
                let tag = if pass == 0 { "cold" } else { "warm" };
                eprintln!(
                    "[{tag}] threads={t:2}: {secs:6.2}s, {outputs:2} outputs, {rate:6.1} files/s, speedup {speedup:4.2}x (events={events})"
                );
            }
        }
        let _ = fs::remove_dir_all(&base);
    }
}
