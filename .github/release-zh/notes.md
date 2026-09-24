mImageViewer 简体中文版（非官方），基于上游 mImageViewer {UPSTREAM_TAG}。

### 新增
- 设置 › 全局设置 › 「表示言語 (Language)」可在 日本語 / 简体中文 之间切换，按 OK 后立即生效，无需重启。首次启动的「初回设置」对话框中也可以选择。
- 已翻译：设置、各类对话框、菜单与快捷键、右键菜单、主界面与工具栏、图片全屏、视频播放界面、元数据面板、搜索、标签、书签、提示消息等常用界面。
- 尚未翻译的部分（调整面板、文字标注、橡皮擦・遮盖编辑、远程访问设置、更新日志等）会显示日文。
- 检查更新改为检查本仓库的发布。

### 下载与使用
- 单文件版：下载 mImageViewer_zh-CN_{TAG}.exe，放在任意位置直接运行。首次启动会把运行文件解压到 %APPDATA%\mimageviewer，设置和缓存也保存在那里（与原版共用设置）。
- 便携版：下载 mImageViewer_zh-CN_portable_{TAG}.zip，解压到可写入的文件夹（不要放在 Program Files），运行 mimageviewer.exe。设置和缓存保存在同一文件夹下的 data 中，不写入 %APPDATA%。
- 本版本没有代码签名，首次运行时 Windows 可能显示 SmartScreen 警告，请选择「更多信息」→「仍要运行」。
- 附带的 .sha256 文件可用于校验下载是否完整。

### 说明
- 程序本体（mimageviewer.exe / mimageviewer-remote.exe / VST3 桥接程序）由本仓库源码通过 GitHub Actions 构建；AI 模型、FFmpeg、PDFium、ONNX Runtime、Susie 工作进程等运行文件与上游 {UPSTREAM_TAG} 便携版相同。
- 原作者：Mikage Sawatari（MIT License）。上游项目：https://github.com/MikageSawatari/mimageviewer
- 本版本与原作者无关，问题请在本仓库反馈。
