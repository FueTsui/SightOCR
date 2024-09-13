![image](https://github.com/user-attachments/assets/9cd3cf79-d9fe-48e5-aa1b-900d96398e48)### SightOCR 视觉字符识别

1. **OCR识别**：
   - 支持 `微信OCR` 和 `百度OCR` 两种接口进行图像文本识别。
   - 通过鼠标框选屏幕区域进行截图，截图后调用 OCR 接口进行文本识别，结果展示在文本框中，并自动复制到剪贴板。
2. **系统托盘图标**：
   - 在程序最小化时，隐藏窗口并通过系统托盘菜单显示主窗口、开始 OCR 识别、打开设置窗口、或者退出程序。
3. **快捷键支持**：
   - 默认快捷键为 `F4`，可以通过设置窗口更改 OCR 启动的快捷键。
4. **设置窗口**：
   - 用户可以在设置窗口中配置 `WeChat` 路径、`WeChatOCR` 路径、`百度OCR` API Key 和 Secret Key，以及自定义 OCR 启动快捷键。
5. **多线程**：
   - 系统托盘图标运行在一个单独的线程中，避免阻塞主线程，从而保持主程序正常响应。

**问题反馈**：
SightOCR 通知: 175332502
![image](https://github.com/user-attachments/assets/77ae558d-6e97-4bfe-ae63-9fa6ec993bb6)


