# heic2img

批量转换 HEIC/HEIF 为 JPG/PNG 的图形界面小工具，纯 Rust 实现。

> 仓库：https://github.com/jackAqwq/heic2img · 使用说明与发布清单：`docs/heic2img-release/heic2img-release.html`

解码基于 [heif-oxide](https://crates.io/crates/heif-oxide)（MIT OR Apache-2.0）——纯 Rust 的 HEIC 解码器，
**无需安装 libheif / vcpkg / HEVC 系统扩展**，单文件 exe 即可运行。
JPEG 编码基于 [jpeg-encoder](https://crates.io/crates/jpeg-encoder)（simd feature，AVX2 加速）。

## 特性

- 图形界面（egui）：添加文件/目录、格式选择、质量滑条、并行线程滑条、进度条、实时日志与吞吐统计
- 固定窗口尺寸（700x640，不可拖拽拉伸、无最大化按钮）
- 无控制台窗口（PE GUI 子系统），双击启动只显示界面
- 支持 iPhone 照片（网格分块图）、10-bit HEVC、Display P3 自动转 sRGB、旋转元数据自动应用
- JPG / PNG / 同时输出两种
- 单文件或目录批量，递归子目录
- JPEG 质量可调（1-100）
- 多线程并行批量转换（默认 = 物理核数，可调 1..逻辑核数）
- 覆盖原文件模式（见下方说明）
- 转换在后台线程执行，不卡界面
- 中文界面与中文文件名支持

## 性能

面向极致吞吐的三层优化：

1. **SIMD JPEG 编码**：jpeg-encoder（AVX2）替换 image 标量编码器，且 RGBA 像素零拷贝直通
   （解码缓冲区直接借用，跳过 RGBA→RGB 转换与整缓冲区拷贝）+ BufWriter 缓冲写盘。
   单张编码+IO：0.395s → **0.015s（26x）**
2. **并行批量**：N 工作线程原子索引领取任务，进度事件按文件序号有序上报。
   heif-oxide 解码无全局状态，可安全并发
3. **默认线程 = 物理核数**：GetLogicalProcessorInformation 检测物理核（超线程对标量
   HEVC 解码无收益）。本机 6C/12T 实测 6 线程最快

本机基准（i5-10400F 6C/12T，Nokia C006 1280x720 样张 × 48，JPG q92，2026-08-19）：

| 配置 | 优化前 | 优化后 |
|------|--------|--------|
| 单线程（文件/秒） | 2.2 | 11.3（5.1x）|
| 默认 6 线程（文件/秒） | 7.5 | **~41（5.5x）** |
| 48 文件总耗时 | 21.9s（1 线程）/ 6.4s（6 线程）| 4.2s / **1.2s** |

整批加速 **12-18x**。当前管线已解码受限（JPG 路径解码占 85% 耗时），
纯 Rust HEVC 解码是剩余瓶颈。PNG 路径采用 zlib Fast 压缩 + RGB 源不扩展 alpha。

> JPEG 质量语义：质量 ≥ 90 时 4:4:4（与旧版一致）；< 90 时 4:2:0 色度抽样
> （文件更小、编码更快，符合 libjpeg 惯例）。

## 体积

单文件 exe **3.9MB**（从 11.7MB 压缩 -67%），转换性能零损失：

| 手段 | 省 | 说明 |
|------|-----|------|
| glow 渲染后端 | -5.5MB | 替换 eframe 默认 wgpu（DX12+Vulkan+GL 三套驱动抽象 + naga 着色器编译器） |
| 去内嵌字体 | -1.4MB | 关 egui default_fonts（NotoEmoji/Ubuntu/Hack/emoji-icon），运行时加载系统微软雅黑 |
| panic=abort + 依赖 opt-level="s" | -1.1MB | 仅 GUI 依赖按体积优化；解码/编码/图像三件套保持 opt-level=3 全速 |
| rfd 关闭无效默认特性 | — | Windows 下 gtk3 特性本就不编译，保持依赖面干净 |

体积与性能不可兼得的坑：`opt-level="z"` 反而比 `"s"` **大 0.6MB**（禁用内联导致 LTO 简化不充分）。
基准复测（48 文件）：单张 decode 0.090s / JPG 编码 0.017s，与优化前一致；GUI 截图确认中文/符号渲染正常。

> 如需进一步压缩可用 UPX（~2MB），但有杀软误报风险，默认不启用。

## 使用

双击 `heic2img.exe` 启动（无命令行参数，纯 GUI）：

1. **添加文件…**：多选 .heic/.heif 文件；**添加目录…**：选择整个目录
2. 输入列表中每项可用 **×** 移除，**清空** 一键移除全部
3. 设置输出格式（JPG / PNG / JPG + PNG）、JPEG 质量、输出目录（留空 = 与源文件同目录）
4. 点击 **开始转换**，查看进度条与逐文件日志

### 覆盖原文件

两个模式由"覆盖原文件"复选框控制：

| 状态 | 同名输出已存在时 | 转换成功后源 HEIC |
|------|------------------|-------------------|
| 未勾选（默认） | 自动加序号（`C006_1.jpg`），不动旧文件 | **保留** |
| 勾选 | 直接覆盖旧文件 | **删除**（输出取代源文件） |

勾选后界面显示红色警告。删除不可恢复，请确认后再启用。

## 编译

```
cargo build --release
```

产物：`target\release\heic2img.exe`

> 本机未安装 MSVC C++ 工具链，使用 GNU 工具链编译（纯 Rust 依赖无 C 代码）。
> GUI 依赖（egui/winit）需要 MinGW binutils 与系统 import library，已备齐于 `F:\AIData\tools\`：
> ```
> rustup toolchain install stable-x86_64-pc-windows-gnu
> # PATH 需包含 F:\AIData\tools\bu\ucrt64\bin（binutils: as/dlltool）
> cargo +stable-x86_64-pc-windows-gnu build --release
> ```
> `.cargo/config.toml` 已配置 `-L F:/AIData/tools/winsyslibs`（40 个系统库 import lib，
> 从 MSYS2 mingw-w64-crt 提取，仅系统库不含 runtime，避免与 rust-mingw 冲突）。

## 测试

`testdata/` 内含 4 个 Nokia HEIF 一致性样张（覆盖 1280x720 / 128x72 / 640x360）和 1 个故意损坏的文件。

```
cargo +stable-x86_64-pc-windows-gnu test --release
```

实测（2026-08-19）：
- 单元测试：7/7 通过
  - 扩展名识别 / 防覆盖序号 / 默认线程数（物理核检测）边界校验
  - 覆盖原文件：勾选后源 HEIC 删除、输出正常生成（含中文文件名）
  - 未勾选：源文件保留、重复转换自动加序号 `_1`
  - 并行批处理 smoke：4 文件 Both 格式，事件流完整、输出齐全
  - 输出有效性：JPG SOI/EOI 标记 + PNG 往返解码尺寸一致
- 基准（`--ignored --nocapture`）：`bench_phases` 分相耗时、`bench_scaling` 线程曲线
- GUI 验证：glow 后端 + 纯系统字体启动正常（窗口可见/响应/中文渲染完整）；
  PE 子系统 = 2（GUI 无控制台）；窗口样式 THICKFRAME/MAXIMIZEBOX OFF（不可拉伸/无最大化）
- 体积：3.9MB（见"体积"章节）

## 项目结构

```
src/
  main.rs   入口：#![windows_subsystem = "windows"] 隐藏控制台，直接启动 GUI
  lib.rs    核心转换逻辑（扫描/解码/编码/并行批处理/物理核检测，含单元测试与基准）
  gui.rs    egui 图形界面（worker 线程 + mpsc 进度事件 + 实时吞吐统计）
docs/
  heic2img-release/  最终使用说明 + 发布清单（自包含 HTML，浏览器直接打开）
```

## 已知限制

- 解码速度约为 libheif 的 1/5（纯 Rust 标量 HEVC），12MP 照片约 1 秒/张（网格分块内部并行）；
  批量场景由并行批次摊薄，当前管线瓶颈在解码
- 不支持 AVIF（`av01`）、HEIF 内嵌 JPEG/H.264、图像序列（live photo 只取主图）
- alpha 辅助图仅支持 4:2:0 编码（iPhone 常规照片不受影响）
- PNG 输出保留 alpha；JPEG 输出丢弃 alpha（JPEG 格式本身不支持）
