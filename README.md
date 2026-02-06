
<img width="200" height="200" alt="icon" src="https://github.com/user-attachments/assets/ddc06b51-cef5-4e16-a293-cb74df508bbe" />

### SightOCR 文字识别翻译

1. **OCR识别**：
   - 默认文本 `oneocr` 精确识别复杂混合多语言文本。
   - 默认表格基于OpenCV和NumPy检测识别技术，识别后可直接使用 Ctrl+V 粘贴到 Excel 中。
   - `百度文本` （高精度、高精度位置、标准、标准位置）、`百度表格`、`百度公式` 。
   - `腾讯文本` （印刷体、高精度）、`腾讯表格`、`腾讯公式` 多种接口进行图像文本识别。
   - `百度文本` 和`腾讯文本` 采用接口轮询自动降级接口处理，如无法处理则采用默认OCR。
   - 通过鼠标框选屏幕区域进行截屏，截屏后调用 OCR 接口进行文本识别，结果自动复制到剪贴板，可直接使用 Ctrl+V 粘贴到任意位置。
2. **翻译**：
   - 支持 `必应` 、 `百度` 和 `腾讯` 三种接口进行文本翻译。
   - 通过鼠标框选屏幕区域进行截屏OCR并翻译，结果自动展示在下方文本框中，并自动复制翻译结果到剪贴板。
   - 翻译源支持 "中文", "English", "日本語", "한국어", "Français", "Deutsch", "Русский", "Español"。
3. **系统托盘图标**：
   - 在程序最小化时，隐藏窗口并通过系统托盘菜单 `主窗口` 、 `识别` 、 `翻译` 、 打开 `设置` 窗口、 `重启` 或者 `退出`程序。
4. **使用方法**：
   - OCR 默认快捷键为 `F4`，可以通过设置窗口更改 OCR 启动的快捷键。
   - 翻译 默认快捷键为 `F2`，可以通过设置窗口更改 翻译 启动的快捷键。
   - 识别文字：快捷键识别到文本后自动复制进剪贴板，通过 Ctrl+V 粘贴到需要的位置即可。
   - 表格、公式识别：需要 `接口` 中选择百度/腾讯对应接口，并配置接口API。
   - 翻译文本：快捷键识别到文本后弹窗等待翻译结果（默认复制结果至剪贴板），通过 Ctrl+V 粘贴到需要的位置即可。
5. **设置窗口**：
   - 用户可以在设置中配置 `百度OCR` 、`百度翻译` 和 `腾讯OCR` 、`腾讯翻译` 接口 ，以及自定义 `OCR、翻译快捷键` 。
   - 支持自定义配置和日志保存路径。
   - 开机启动开关、显示和隐藏系统托盘图标开关。
6. **配置文件**：
   - config.json 用于加载和保存配置文件。
     
<img width="338" height="359" alt="main" src="https://github.com/user-attachments/assets/eb3d5bb1-2858-47f1-9236-27bb3d1edbde" />




   **下载**： [百度云盘](https://pan.baidu.com/s/1NuPCQd0tJH0ZXSFqld5Cfg?pwd=2020)  提取码: 2020 
   
   **摸鱼搭子**：*QQ群*  [175332502](http://qm.qq.com/cgi-bin/qm/qr?_wv=1027&k=5PkXysHbDS-RVvXJO_AE7OkFEZrIaFYN&authKey=OZ7pUAm4Ek2ZUKmurSP5v2w9lcbQN2%2BnoiGdIJzM0ZT5QsFkrV0CICvkw9C7qIWS&noverify=0&group_code=175332502)	

<img width="300" height="300" alt="pay" src="https://github.com/user-attachments/assets/c42d823d-3165-4129-ac8f-bdffacbdc1a6" />




