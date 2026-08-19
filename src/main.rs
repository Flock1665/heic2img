// 隐藏 Windows 控制台窗口：PE 子系统切换为 GUI（当前代码无 stdout 输出，安全）
#![windows_subsystem = "windows"]

fn main() -> eframe::Result<()> {
    heic2img::gui::run()
}
