// 构建脚本：将 assets/icon.ico 嵌入 exe 资源（文件图标），
// 任务栏窗口图标由 main.rs 的 load_icon() 在运行时设置。
fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.compile().unwrap();
}
