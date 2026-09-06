import sys
import os
import subprocess

# 防止第三方 C 库（numpy/scipy/cv2/sklearn）在 PyInstaller 打包的窗口应用中
# 首次初始化时弹出控制台窗口（Windows 11 下表现为 PowerShell 闪烁）
os.environ.setdefault('OPENBLAS_NUM_THREADS', '1')
os.environ.setdefault('MKL_NUM_THREADS', '1')
os.environ.setdefault('OPENCV_OPENCL_RUNTIME', '')       # 禁用 OpenCV OpenCL 运行时加载
os.environ.setdefault('OPENCV_OPENCL_DEVICE', 'disabled') # 禁用 OpenCV OpenCL 设备检测
os.environ.setdefault('LOKY_MAX_CPU_COUNT', '1')          # 控制 sklearn/joblib 进程池

# PyInstaller 打包环境下，全局拦截 subprocess.Popen 防止任何子进程弹出控制台窗口
if sys.platform == 'win32' and getattr(sys, 'frozen', False):
    _orig_popen_init = subprocess.Popen.__init__
    def _no_console_popen_init(self, *args, **kwargs):
        if 'creationflags' not in kwargs:
            kwargs['creationflags'] = subprocess.CREATE_NO_WINDOW
        if kwargs.get('startupinfo') is None:
            si = subprocess.STARTUPINFO()
            si.dwFlags |= subprocess.STARTF_USESHOWWINDOW
            si.wShowWindow = subprocess.SW_HIDE
            kwargs['startupinfo'] = si
        _orig_popen_init(self, *args, **kwargs)
    subprocess.Popen.__init__ = _no_console_popen_init

import threading
import multiprocessing
import time
import win32gui
import win32con
import win32api
import ctypes
from ctypes import wintypes
from PySide6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QLabel, QPushButton, QFormLayout,
    QLineEdit, QComboBox, QTextEdit, QMessageBox, QSystemTrayIcon, QMenu,
    QHBoxLayout, QVBoxLayout, QToolButton, QGridLayout, QFrame, QSplitter,
    QAbstractItemView
)
from PySide6.QtGui import QIcon, QAction, QActionGroup
from PySide6.QtCore import Qt, Signal, QTimer, QThread, QMetaObject, Q_ARG, QObject, QEvent
from src.utils import disable_dpi_scaling, resource_path, run_as_admin, trim_working_set
from src.managers import ThemeManager, ConfigManager, TrayManager, OCRManager, TranslationManager
from src.perf_trace import TranslateTracer
from src.paths import get_log_dir
import logging
from logging.handlers import RotatingFileHandler

# Determine the log file path
log_dir = get_log_dir()

os.makedirs(log_dir, exist_ok=True)
log_file_path = os.path.join(log_dir, 'sightocr.log')

# 通过环境变量 SIGHTOCR_DEBUG=1 可临时开启 DEBUG 级别排障；
# 默认 INFO，避免记录翻译文本片段等本地隐私信息且降低日志体积。
_log_level = logging.DEBUG if os.environ.get('SIGHTOCR_DEBUG') == '1' else logging.INFO

# 使用轮转文件处理器，单文件最大 2MB，保留 3 个备份，防止日志无限增长
_rotating_handler = RotatingFileHandler(
    log_file_path,
    maxBytes=2 * 1024 * 1024,
    backupCount=3,
    encoding='utf-8'
)
_rotating_handler.setFormatter(
    logging.Formatter('%(asctime)s - %(levelname)s - %(message)s')
)

logging.basicConfig(
    level=_log_level,
    handlers=[_rotating_handler],
    force=True  # 强制重新配置，覆盖可能被其他库预先设置的 handlers
)

# 系统级注册的窗口消息，用于从第二个实例激活主窗口。
# RegisterWindowMessage 对同一字符串在所有进程中返回相同的消息 ID（0xC000+），
# 全系统唯一，不会与其他应用或 Qt 内部的 WM_USER 消息冲突。
# 理论上注册失败返回 0（即 WM_NULL，会被频繁触发），故回退到 WM_USER 段
WM_SHOW_SELF = win32gui.RegisterWindowMessage("SightOCR_ShowSelf") or (win32con.WM_USER + 100)

# --- 单实例检测 (Named Mutex) ---
# 使用 WinDLL + use_last_error 确保 GetLastError 被正确捕获
_kernel32_si = ctypes.WinDLL('kernel32', use_last_error=True)
_kernel32_si.CreateMutexW.restype = wintypes.HANDLE
_kernel32_si.CreateMutexW.argtypes = [wintypes.LPVOID, wintypes.BOOL, wintypes.LPCWSTR]
_kernel32_si.CloseHandle.argtypes = [wintypes.HANDLE]
_kernel32_si.CloseHandle.restype = wintypes.BOOL

_MUTEX_NAME = "Local\\SightOCR_SingleInstance"
_ERROR_ALREADY_EXISTS = 183
_ERROR_ACCESS_DENIED = 5
_instance_mutex = None


def _try_acquire_single_instance():
    """尝试获取单实例互斥锁。
    返回 True 表示当前是第一个实例，False 表示已有实例运行。
    """
    global _instance_mutex
    handle = _kernel32_si.CreateMutexW(None, False, _MUTEX_NAME)
    last_error = ctypes.get_last_error()

    if not handle:
        if last_error == _ERROR_ACCESS_DENIED:
            # Mutex 已存在但当前进程无权限打开（如由不同完整性级别的实例创建），
            # 同样视为已有实例在运行
            return False
        # Mutex 创建完全失败（系统级错误），允许启动以免阻塞用户
        logging.error(f"CreateMutexW 失败，错误码: {last_error}")
        return True

    if last_error == _ERROR_ALREADY_EXISTS:
        # 已有实例持有此 Mutex
        _kernel32_si.CloseHandle(handle)
        return False

    # 成功创建，当前是第一个实例
    _instance_mutex = handle
    return True


def _release_single_instance():
    """释放单实例互斥锁。"""
    global _instance_mutex
    if _instance_mutex:
        _kernel32_si.CloseHandle(_instance_mutex)
        _instance_mutex = None


def _activate_existing_instance():
    """通知已有实例将主窗口前置显示。

    通过 PostMessage(HWND_BROADCAST) 广播注册消息，由已有实例的主窗口在
    nativeEvent 中识别并响应。不按标题 FindWindow：标题匹配会误中其他程序中
    恰好同名的窗口，且窗口一旦改名机制即失效。
    广播本身无法确认送达，重发数次以覆盖两个实例几乎同时启动、
    对方主窗口尚未创建完成的窗口期；重复送达只会幂等地重复前置窗口。
    """
    for attempt in range(3):
        if attempt:
            time.sleep(0.5)
        try:
            win32api.PostMessage(win32con.HWND_BROADCAST, WM_SHOW_SELF, 0, 0)
        except Exception as e:
            logging.warning(f"广播激活消息失败: {e}")
            return False
    return True


# 自定义 QTextEdit 类，添加中文右键菜单
class CustomTextEdit(QTextEdit):
    # 在类级别定义信号
    translate_requested = Signal()
    
    def __init__(self, parent=None):
        super().__init__(parent)
    
    def contextMenuEvent(self, event):
        menu = QMenu(self)
        
        # 创建中文菜单项
        undo_action = QAction("撤销", self)
        undo_action.setShortcut("Ctrl+Z")
        undo_action.triggered.connect(self.undo)
        
        redo_action = QAction("重做", self)
        redo_action.setShortcut("Ctrl+Y")
        redo_action.triggered.connect(self.redo)
        
        cut_action = QAction("剪切", self)
        cut_action.setShortcut("Ctrl+X")
        cut_action.triggered.connect(self.cut)
        
        copy_action = QAction("复制", self)
        copy_action.setShortcut("Ctrl+C")
        copy_action.triggered.connect(self.copy)
        
        paste_action = QAction("粘贴", self)
        paste_action.setShortcut("Ctrl+V")
        paste_action.triggered.connect(self.paste)
        
        delete_action = QAction("删除", self)
        delete_action.triggered.connect(self.deleteSelected)
        
        select_all_action = QAction("全选", self)
        select_all_action.setShortcut("Ctrl+A")
        select_all_action.triggered.connect(self.selectAll)
        
        # 添加菜单项到菜单
        menu.addAction(undo_action)
        menu.addAction(redo_action)
        menu.addSeparator()
        menu.addAction(cut_action)
        menu.addAction(copy_action)
        menu.addAction(paste_action)
        menu.addAction(delete_action)
        menu.addSeparator()
        menu.addAction(select_all_action)
        
        # 显示菜单
        menu.exec_(event.globalPos())
    
    def deleteSelected(self):
        cursor = self.textCursor()
        cursor.removeSelectedText()
        
    def keyPressEvent(self, event):
        # 检查是否按下了Enter键
        if event.key() == Qt.Key_Return or event.key() == Qt.Key_Enter:
            # 如果文本框中有内容，发出信号
            if self.toPlainText().strip():
                self.translate_requested.emit()
                return  # 不继续处理Enter键事件
        # 调用父类方法处理其他按键事件
        super().keyPressEvent(event)

class ComboPopupOnClickFilter(QObject):
    """把语言下拉框只读 lineEdit 上的左键按下转发为弹出列表。

    不能直接对 lineEdit 实例补丁 mousePressEvent：QComboBox 给自己的
    lineEdit 安装了事件过滤器，鼠标按下在到达虚函数前就被拦截，补丁
    的处理函数永远不会执行。事件过滤器后装先执行，可抢在其之前处理。

    直接调用 showPopup() 会绕过 QComboBox 私有方法 showPopupFromMouseEvent
    中的 blockMouseReleaseTimer 保护：弹出层是抓取鼠标的 popup 窗口，
    打开点击自身的"释放"会被它收到并立即触发关闭，表现为列表偶尔
    一闪而过、需要多次点击。该保护无法从 Python 调用，故在此复刻：

    1. 弹出后 doubleClickInterval 内落在弹出层本体（列表项之外）的
       鼠标释放一律吞掉，打开点击的释放不再误关列表；
    2. 记录弹出层关闭时刻：列表因外部点击关闭时 Qt 会把该按下重放给
       lineEdit，刚关闭 REOPEN_SUPPRESS_MS 内的按下不再重新弹出，
       使"再点一次语言框收起列表"成立；
    3. 双击事件与按下同样处理，快速连点不会出现无响应的空拍。"""

    REOPEN_SUPPRESS_MS = 150

    def __init__(self, combo):
        super().__init__(combo)
        self._combo = combo
        # view() 会强制创建弹出层容器（QComboBoxPrivateContainer）
        self._container = combo.view().window()
        self._container.installEventFilter(self)
        self._popup_opened_at = 0.0
        self._popup_hidden_at = 0.0

    def eventFilter(self, obj, event):
        etype = event.type()

        if obj is self._container:
            if etype == QEvent.Hide:
                self._popup_hidden_at = time.monotonic()
            elif (etype == QEvent.MouseButtonRelease
                    and (time.monotonic() - self._popup_opened_at) * 1000
                        < QApplication.doubleClickInterval()):
                return True
            return super().eventFilter(obj, event)

        # lineEdit 上的事件
        if (etype in (QEvent.MouseButtonPress, QEvent.MouseButtonDblClick)
                and event.button() == Qt.LeftButton):
            if ((time.monotonic() - self._popup_hidden_at) * 1000
                    < self.REOPEN_SUPPRESS_MS):
                return True
            self._combo.showPopup()
            self._popup_opened_at = time.monotonic()
            return True
        return super().eventFilter(obj, event)


class MainWindow(QMainWindow):
    ocr_result_signal = Signal(str, bool, bool)
    translation_done = Signal(str)
    error_signal = Signal(str, str)
    ocr_selection_changed_signal = Signal(str)
    translation_selection_changed_signal = Signal(str)
    ocr_request_signal = Signal(bool, bool)
    hotkey_translate_signal = Signal()

    # 旧版本语言栏使用本族语显示名，读取旧配置时映射到新的中文显示名
    LEGACY_LANG_NAMES = {
        "auto": "自动识别",
        "中文": "简体中文",
        "English": "英语",
        "日本語": "日语",
        "한국어": "韩语",
        "Français": "法语",
        "Deutsch": "德语",
        "Русский": "俄语",
        "Español": "西班牙语",
    }

    def _normalize_lang_name(self, name, default):
        """规范化配置中的语言显示名：兼容旧版名称，无效值回退到默认。

        语言下拉框为可编辑模式（为实现文字居中），setCurrentText 对
        不存在的条目会直接显示任意文本，因此必须在恢复前校验。"""
        name = self.LEGACY_LANG_NAMES.get(name, name)
        return name if name in self.lang_codes else default

    def __init__(self):
        super().__init__()
        self.setWindowTitle("SightOCR")
        icon_path = resource_path("assets/icon.png")
        self.setWindowIcon(QIcon(icon_path))
        # 固定窗口尺寸（宽450 × 高421）。
        # 必须显式设置 MSWindowsFixedSizeDialogHint：PySide6 6.8+ 不再因
        # setFixedSize 自动移除 WS_MAXIMIZEBOX（6.7.3 会），缺少此标志时
        # 打包环境（venv 6.10.2）下最大化按钮仍可点击
        self.setFixedSize(450, 421)
        self.setWindowFlag(Qt.MSWindowsFixedSizeDialogHint, True)

        # Initialize managers (singletons)
        self._config = ConfigManager.instance()
        self._theme_mgr = ThemeManager.instance()

        self.init_variables()
        self.init_ui()
        self._purge_window_state_config()

        # 确保在所有UI初始化完成后加载用户选择
        self.load_last_selections()  # 加载上次的选择

        # Defer non-critical initializations to speed up startup
        QTimer.singleShot(100, self.deferred_init)

        # 更新信号连接
        self.ocr_result_signal.connect(self.display_ocr_result_and_translate)
        self.translation_done.connect(self.on_translation_done)
        self.error_signal.connect(self.show_error_message)
        self.ocr_selection_changed_signal.connect(self.update_ocr_selection_ui)
        self.translation_selection_changed_signal.connect(self.update_translation_selection_ui)
        self.hotkey_translate_signal.connect(self._handle_hotkey_translate, Qt.QueuedConnection)

        # Initialize OCR and Translation managers
        self._ocr_mgr = OCRManager(self)
        self._ocr_mgr.ocr_completed.connect(self.display_ocr_result_and_translate)
        self._ocr_mgr.ocr_error.connect(self.show_error_message)
        self._ocr_mgr.ocr_selection_changed.connect(self.update_ocr_selection_ui)
        self._ocr_mgr.start()
        self.ocr_request_signal.connect(self._dispatch_hotkey_ocr_request, Qt.QueuedConnection)
        self._ocr_mgr.pre_init_default_ocr()

        self._trans_mgr = TranslationManager(self)
        self._trans_mgr.translation_completed.connect(self.on_translation_done)
        self._trans_mgr.translation_error.connect(self.show_error_message)
        self._trans_mgr.translation_selection_changed.connect(self.update_translation_selection_ui)
        self._trans_mgr.warm_up_cache()  # 尽早预热 Edge token，减少首次翻译等待


    def nativeEvent(self, eventType, message):
        # 处理 Windows 原生消息, 用于单实例应用
        try:
            if eventType == b"windows_generic_MSG":
                msg = wintypes.MSG.from_address(message.__int__())
                if msg.message == WM_SHOW_SELF:
                    # 收到来自第二个实例的激活信号（保留最大化状态）
                    self._show_preserving_state()
                    self.activateWindow()
                    self.raise_()
                    return True, 0
        except Exception as e:
            logging.warning(f"处理原生消息时出错: {e}")
        return super().nativeEvent(eventType, message)

    def deferred_init(self):
        """
        在主窗口显示后执行非关键的初始化任务，以加快启动速度。
        """
        logging.debug("Performing deferred initializations...")
        # 禁用窗口隐藏淡出动画，确保“识别”截图时主窗口瞬时消失、不挡住待识别内容
        self._disable_window_transitions()
        self.init_hotkey_handler()
        self.init_session_monitor()
        from src.screenshot import start_cleanup_thread
        start_cleanup_thread()
        self.init_tray_icon()
        self._start_background_health_monitor()
        self._start_theme_monitor()  # 启动主题监控
        # 启动翻译保活：默认翻译时定期保持 token 与连接“热”，消除空闲后首次翻译的冷重连延迟
        if hasattr(self, '_trans_mgr'):
            self._trans_mgr.start_keepalive()
        # 预热截图选择器：在工作线程上提前创建 Tk 根窗口并预热 ImageTk/mss，
        # 消除开机自启后首次按热键截图约 3 秒的冷启动延迟
        if hasattr(self, '_ocr_mgr'):
            self._ocr_mgr.request_selector_prewarm()
        # 静默启动（--silent，开机自启场景）下窗口始终在后台：
        # 等各项预热完成后收缩一次工作集，降低后台常驻内存
        QTimer.singleShot(30000, self._trim_if_backgrounded)
        logging.debug("Deferred initializations complete.")

    def _trim_if_backgrounded(self):
        """仅当主窗口处于后台（隐藏）时收缩工作集。"""
        if self._is_shutting_down or self.isVisible():
            return
        if trim_working_set():
            logging.debug("后台驻留，已收缩进程工作集")

    def closeEvent(self, event):
        if getattr(self, '_is_shutting_down', False):
            event.accept()
            return

        logging.debug("closeEvent triggered")
        self.save_selections()  # 在关闭窗口时保存选择
        settings_to_save = {
            'api_key': self.api_key_var,
            'secret_key': self.secret_key_var,
            'baidu_trans_appid': self.baidu_trans_appid_var,
            'baidu_trans_appkey': self.baidu_trans_appkey_var,
            'tencent_secret_id': self.tencent_secret_id_var,
            'tencent_secret_key': self.tencent_secret_key_var,
            'tencent_trans_secret_id': self.tencent_trans_secret_id_var,
            'tencent_trans_secret_key': self.tencent_trans_secret_key_var,
            'hotkey': self.hotkey_var,
            'translate_hotkey': self.translate_hotkey_var,
            'hide_tray_icon': self.hide_tray_icon_var,
            'replace_newline': self.replace_newline_var
        }
        self.save_settings(settings_to_save)
        self.hide()
        # 进入后台驻留：稍后将工作集交还系统，降低任务管理器中的内存占用。
        # 延迟执行以等待隐藏与配置保存完成；页面进入备用列表，热键唤醒时
        # 软缺页取回，无磁盘 IO，不影响响应速度。
        QTimer.singleShot(1000, self._trim_if_backgrounded)

        event.ignore()

    def _purge_window_state_config(self):
        """清理旧版本写入 config.json 的窗口状态字段。

        窗口位置与分割条位置均不持久化：窗口每次启动出现在系统默认位置，
        识别区/翻译区恢复 init_ui 中设定的默认等高布局。仅当配置中存在
        遗留字段时才写盘一次，此后启动为空操作。
        """
        try:
            if (self._config.get('window_geometry') is not None
                    or self._config.get('splitter_state') is not None):
                self._config.remove('window_geometry', auto_save=False)
                self._config.remove('splitter_state', auto_save=False)
                self._config.save()
        except Exception as e:
            logging.debug(f"清理窗口状态配置时出错: {e}")

    def _show_preserving_state(self):
        """显示窗口并保留最大化状态（showNormal 会把最大化打回普通尺寸）。"""
        if self.isMaximized():
            self.showMaximized()
        else:
            self.showNormal()

    def init_variables(self):
        """Initialize instance variables from ConfigManager."""
        logging.debug("配置文件加载完成")

        # Use ConfigManager properties instead of globals
        self.api_key_var = self._config.api_key
        self.secret_key_var = self._config.secret_key
        self.baidu_trans_appid_var = self._config.baidu_trans_appid
        self.baidu_trans_appkey_var = self._config.baidu_trans_appkey
        self.tencent_secret_id_var = self._config.tencent_secret_id
        self.tencent_secret_key_var = self._config.tencent_secret_key
        self.tencent_trans_secret_id_var = self._config.tencent_trans_secret_id
        self.tencent_trans_secret_key_var = self._config.tencent_trans_secret_key
        self.hotkey_var = self._config.hotkey
        self.translate_hotkey_var = self._config.translate_hotkey
        self.hide_tray_icon_var = self._config.hide_tray_icon
        self.replace_newline_var = self._config.replace_newline

        logging.debug("变量初始化完成")

        # OCR and translate selections
        self.last_ocr_selection = self._config.last_ocr_selection
        if not self.last_ocr_selection:
            self.last_ocr_selection = '默认'
            self._config.last_ocr_selection = '默认'
            self._config.save()
            logging.debug("首次启动，OCR接口设置为默认")
        else:
            logging.debug(f"从配置文件加载OCR选择: {self.last_ocr_selection}")

        self.last_translate_selection = self._config.last_translate_selection

        # 内部语言码采用 Edge 翻译（BCP-47）代码，百度/腾讯接口各自在
        # translate.py 中按需转换；显示顺序即下拉列表顺序
        self.languages = {
            "auto": "自动识别",
            "zh-Hans": "简体中文",
            "zh-Hant": "繁体中文",
            "yue": "中文粤语",
            "en": "英语",
            "ja": "日语",
            "ko": "韩语",
            "fr": "法语",
            "es": "西班牙语",
            "ru": "俄语",
            "de": "德语",
            "it": "意大利语",
            "tr": "土耳其语",
            "pt-PT": "葡萄牙语",
            "pt": "巴西葡萄牙语",
            "vi": "越南语",
            "id": "印度尼西亚语",
            "th": "泰语",
            "ms": "马来语",
            "ar": "阿拉伯语",
            "hi": "印地语",
            "mn-Cyrl": "蒙古语(西里尔)",
            "mn-Mong": "蒙古语",
            "km": "高棉语",
            "nb": "书面挪威语",
            "nn": "新挪威语",
            "fa": "波斯语",
            "sv": "瑞典语",
            "pl": "波兰语",
            "nl": "荷兰语",
            "uk": "乌克兰语",
            "uz": "乌兹别克语",
        }
        self.lang_codes = {v: k for k, v in self.languages.items()}

        self.last_source_lang = self._normalize_lang_name(
            self._config.last_source_lang, default="自动识别")
        self.last_target_lang = self._normalize_lang_name(
            self._config.last_target_lang, default="简体中文")
        self._tracer = TranslateTracer()
        self._is_shutting_down = False

        # 记录日志，帮助调试
        logging.debug(f"最终使用的OCR选择: {self.last_ocr_selection}")
        logging.debug(f"最终使用的翻译选择: {self.last_translate_selection}")

    def _is_dark_theme(self):
        """检测当前是否为深色主题（使用 ThemeManager）"""
        return self._theme_mgr.is_dark_theme()

    def _start_theme_monitor(self):
        """启动主题监控 - 连接到 ThemeManager 信号"""
        self._theme_mgr.theme_changed.connect(self._on_theme_changed)
        self._theme_mgr.start_monitoring()

    def _on_theme_changed(self, is_dark: bool):
        """Handle theme change from ThemeManager."""
        self._apply_theme_style(force=True)
        if self.always_on_top:
            self._update_toggle_button_style()

    def _apply_theme_style(self, force=False):
        """根据当前主题应用样式"""
        is_dark = self._theme_mgr.is_dark_theme()

        # 如果主题没有改变，跳过重新应用
        if not force and hasattr(self, '_current_theme_dark') and self._current_theme_dark == is_dark:
            return

        self._current_theme_dark = is_dark
        self.setStyleSheet(self._theme_mgr.get_main_window_stylesheet())

    def changeEvent(self, event):
        """监听系统主题变化"""
        if event.type() == event.Type.PaletteChange:
            # ThemeManager handles monitoring, but we can also respond to palette changes
            self._apply_theme_style()
            if self.always_on_top:
                self._update_toggle_button_style()
        super().changeEvent(event)

    def _update_toggle_button_style(self):
        """更新置顶按钮样式以适应主题"""
        if self.always_on_top:
            self.always_on_top_button.setStyleSheet(self._theme_mgr.get_toggle_button_active_style())

    def init_ui(self):
        central_widget = QWidget()
        self.setCentralWidget(central_widget)

        # 应用主题样式
        self._apply_theme_style()

        # ================= 主布局 =================
        main_layout = QVBoxLayout(central_widget)
        main_layout.setContentsMargins(12, 12, 12, 12)
        main_layout.setSpacing(10)

        # ================= 顶部按钮区 =================
        top_layout = QHBoxLayout()
        top_layout.setSpacing(10)
        main_layout.addLayout(top_layout)

        # 置顶按钮 - 宽度适应"取消"文字
        self.always_on_top = False
        self.always_on_top_button = QPushButton("置顶")
        self.always_on_top_button.setFixedHeight(32)
        self.always_on_top_button.setMinimumWidth(80)
        self.always_on_top_button.clicked.connect(self.toggle_always_on_top)
        top_layout.addWidget(self.always_on_top_button)

        top_layout.addStretch(1)  # 弹簧间距

        # 接口菜单
        self.interfaces_button = QToolButton()
        self.interfaces_button.setText("接口")
        self.interfaces_button.setFixedHeight(32)
        self.interfaces_button.setMinimumWidth(80)
        self.interfaces_button.setPopupMode(QToolButton.InstantPopup)
        interfaces_menu = QMenu(self)
        self.interfaces_button.setMenu(interfaces_menu)

        # OCR 子菜单
        ocr_menu = interfaces_menu.addMenu("识别源")
        self.ocr_group = QActionGroup(self)
        self.ocr_group.setExclusive(True)

        default_menu = ocr_menu.addMenu("默认")

        self.default_text_action = QAction("文本", self, checkable=True)
        self.default_text_action.triggered.connect(lambda: self.on_ocr_selection_changed("默认"))
        default_menu.addAction(self.default_text_action)
        self.ocr_group.addAction(self.default_text_action)

        self.default_table_action = QAction("表格", self, checkable=True)
        self.default_table_action.triggered.connect(lambda: self.on_ocr_selection_changed("默认_table"))
        default_menu.addAction(self.default_table_action)
        self.ocr_group.addAction(self.default_table_action)

        # Baidu submenu
        baidu_menu = ocr_menu.addMenu("百度")

        self.baidu_text_auto_action = QAction("文本", self, checkable=True)
        self.baidu_text_auto_action.triggered.connect(lambda: self.on_ocr_selection_changed("Baidu_auto"))
        baidu_menu.addAction(self.baidu_text_auto_action)
        self.ocr_group.addAction(self.baidu_text_auto_action)

        self.baidu_table_action = QAction("表格", self, checkable=True)
        self.baidu_table_action.triggered.connect(lambda: self.on_ocr_selection_changed("Baidu_table"))
        baidu_menu.addAction(self.baidu_table_action)
        self.ocr_group.addAction(self.baidu_table_action)

        self.baidu_formula_action = QAction("公式", self, checkable=True)
        self.baidu_formula_action.triggered.connect(lambda: self.on_ocr_selection_changed("Baidu_formula"))
        baidu_menu.addAction(self.baidu_formula_action)
        self.ocr_group.addAction(self.baidu_formula_action)

        self.baidu_actions = {
            "Baidu_auto": self.baidu_text_auto_action,
            "Baidu_table": self.baidu_table_action,
            "Baidu_formula": self.baidu_formula_action
        }

        # Tencent submenu
        tencent_menu = ocr_menu.addMenu("腾讯")

        self.tencent_text_auto_action = QAction("文本", self, checkable=True)
        self.tencent_text_auto_action.triggered.connect(lambda: self.on_ocr_selection_changed("Tencent_auto"))
        tencent_menu.addAction(self.tencent_text_auto_action)
        self.ocr_group.addAction(self.tencent_text_auto_action)

        self.tencent_table_action = QAction("表格", self, checkable=True)
        self.tencent_table_action.triggered.connect(lambda: self.on_ocr_selection_changed("Tencent_table"))
        tencent_menu.addAction(self.tencent_table_action)
        self.ocr_group.addAction(self.tencent_table_action)

        self.tencent_formula_action = QAction("公式", self, checkable=True)
        self.tencent_formula_action.triggered.connect(lambda: self.on_ocr_selection_changed("Tencent_formula"))
        tencent_menu.addAction(self.tencent_formula_action)
        self.ocr_group.addAction(self.tencent_formula_action)

        self.tencent_actions = {
            "Tencent_auto": self.tencent_text_auto_action,
            "Tencent_table": self.tencent_table_action,
            "Tencent_formula": self.tencent_formula_action
        }

        # 翻译源子菜单
        trans_menu = interfaces_menu.addMenu("翻译源")
        trans_group = QActionGroup(self)
        trans_group.setExclusive(True)

        self.bing_action = QAction("默认", self, checkable=True)
        self.bing_action.triggered.connect(lambda: self.on_translate_selection_changed("默认"))
        trans_menu.addAction(self.bing_action)
        trans_group.addAction(self.bing_action)

        self.baidu_trans_action = QAction("百度", self, checkable=True)
        self.baidu_trans_action.triggered.connect(lambda: self.on_translate_selection_changed("Baidu"))
        trans_menu.addAction(self.baidu_trans_action)
        trans_group.addAction(self.baidu_trans_action)

        self.tencent_trans_action = QAction("腾讯", self, checkable=True)
        self.tencent_trans_action.triggered.connect(lambda: self.on_translate_selection_changed("Tencent"))
        trans_menu.addAction(self.tencent_trans_action)
        trans_group.addAction(self.tencent_trans_action)

        top_layout.addWidget(self.interfaces_button)

        top_layout.addStretch(1)  # 弹簧间距

        # 识别按钮
        self.start_ocr_button = QPushButton("识别")
        self.start_ocr_button.setFixedHeight(32)
        self.start_ocr_button.setMinimumWidth(80)
        self.start_ocr_button.clicked.connect(self.threaded_start_ocr)
        top_layout.addWidget(self.start_ocr_button)

        top_layout.addStretch(1)  # 弹簧间距

        # 翻译按钮
        translate_button = QPushButton("翻译")
        translate_button.setFixedHeight(32)
        translate_button.setMinimumWidth(80)
        translate_button.clicked.connect(self.translate_text)
        top_layout.addWidget(translate_button)

        top_layout.addStretch(1)  # 弹簧间距

        # 设置按钮
        settings_button = QPushButton("设置")
        settings_button.setFixedHeight(32)
        settings_button.setMinimumWidth(80)
        settings_button.clicked.connect(self.open_settings_window)
        top_layout.addWidget(settings_button)

        # ================= 使用 QSplitter 实现可拖动的文本区 =================
        self.splitter = QSplitter(Qt.Vertical)
        main_layout.addWidget(self.splitter, 1)

        # OCR 显示区
        self.result_box = CustomTextEdit()
        self.result_box.setPlaceholderText("识别显示区...")
        self.result_box.translate_requested.connect(self.translate_text)
        self.splitter.addWidget(self.result_box)

        # ================= 语言选择区 =================
        lang_widget = QWidget()
        lang_widget.setFixedHeight(36)
        lang_layout = QHBoxLayout(lang_widget)
        lang_layout.setContentsMargins(0, 4, 0, 4)
        lang_layout.setSpacing(12)

        self.source_lang_combo = QComboBox()
        self.target_lang_combo = QComboBox()

        source_langs = list(self.languages.values())
        self.source_lang_combo.addItems(source_langs)

        target_langs = list(self.languages.values())
        target_langs.remove("自动识别")
        self.target_lang_combo.addItems(target_langs)

        # 两框始终取相同宽度（见 _sync_lang_combo_widths），转换符号两侧视觉对称；
        # 弹出列表单独设最小宽度，保证长语言名不被截断
        for combo in (self.source_lang_combo, self.target_lang_combo):
            combo.setFixedHeight(26)

            # 主题已隐藏下拉箭头，框体是纯文字药丸；QComboBox 默认左对齐绘制
            # 文字，固定宽度下空白全堆在右侧。借助只读 lineEdit 实现文字居中
            # （Qt 无法直接居中非编辑态 QComboBox 的文字），点击转发为弹出列表
            combo.setEditable(True)
            line = combo.lineEdit()
            line.setReadOnly(True)
            line.setAlignment(Qt.AlignCenter)
            line.setStyleSheet("background: transparent; border: none;")
            line.setCursor(Qt.ArrowCursor)
            line.installEventFilter(ComboPopupOnClickFilter(combo))

            fm = combo.fontMetrics()
            popup_width = max(
                fm.horizontalAdvance(combo.itemText(i)) for i in range(combo.count())
            ) + 32
            combo.view().setMinimumWidth(popup_width)
            # 语种较多时弹出列表不显示滚动条，改用滚轮按像素平滑滑动浏览
            combo.view().setVerticalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
            combo.view().setVerticalScrollMode(QAbstractItemView.ScrollPerPixel)
            # 弹出列表项同步居中，与框内文字观感一致
            for i in range(combo.count()):
                combo.setItemData(i, Qt.AlignCenter, Qt.TextAlignmentRole)
            combo.currentTextChanged.connect(self._sync_lang_combo_widths)
        self._sync_lang_combo_widths()

        self.source_lang_combo.currentTextChanged.connect(self.on_lang_selection_changed)
        self.target_lang_combo.currentTextChanged.connect(self.on_lang_selection_changed)

        # 两个下拉框各放进一个等比伸缩的容器（左框右对齐、右框左对齐），
        # 转换符号位于两容器正中间，居中与下拉框宽度无关
        source_wrap = QWidget()
        source_wrap_layout = QHBoxLayout(source_wrap)
        source_wrap_layout.setContentsMargins(0, 0, 0, 0)
        source_wrap_layout.addStretch()
        source_wrap_layout.addWidget(self.source_lang_combo)

        target_wrap = QWidget()
        target_wrap_layout = QHBoxLayout(target_wrap)
        target_wrap_layout.setContentsMargins(0, 0, 0, 0)
        target_wrap_layout.addWidget(self.target_lang_combo)
        target_wrap_layout.addStretch()

        lang_layout.addWidget(source_wrap, 1)
        # 语言框与转换符号边距 = 28px 间隔 + 12px 默认 spacing = 40px
        lang_layout.addSpacing(28)

        # U+276F：单侧尖括号，字形居中且左右留白对称，
        # 避免 U+2192 全宽字形导致的视觉偏移
        self.arrow_label = QLabel("❯")
        self.arrow_label.setAlignment(Qt.AlignCenter)
        self.arrow_label.setStyleSheet("font-size: 12px; background: transparent;")
        lang_layout.addWidget(self.arrow_label)

        lang_layout.addSpacing(28)
        lang_layout.addWidget(target_wrap, 1)

        # ================= 下半部：语言栏 + 翻译显示区 =================
        # 语言栏不直接作为 splitter 面板（固定高度的面板会多出一条无效手柄），
        # 而是与翻译区合为一个容器，splitter 只保留一条手柄
        self.translate_box = CustomTextEdit()
        self.translate_box.setPlaceholderText("翻译显示区...")
        self.translate_box.translate_requested.connect(self.translate_text)

        bottom_widget = QWidget()
        bottom_layout = QVBoxLayout(bottom_widget)
        bottom_layout.setContentsMargins(0, 0, 0, 0)
        bottom_layout.setSpacing(4)
        bottom_layout.addWidget(lang_widget)
        bottom_layout.addWidget(self.translate_box, 1)
        self.splitter.addWidget(bottom_widget)

        # 下半面板比翻译框多出的固定高度（语言栏 + 面板内间距），
        # 等高计算时从总高中扣除；构造期 splitter 尚无有效几何，
        # 等高布局在首次 showEvent 中按实际尺寸应用
        self._splitter_bottom_extra = (
            lang_widget.maximumHeight() + bottom_layout.spacing()
        )
        self._splitter_equalized = False
        self.splitter.setStretchFactor(0, 1)
        self.splitter.setStretchFactor(1, 1)
        self.splitter.setCollapsible(0, False)
        self.splitter.setCollapsible(1, False)

    def toggle_always_on_top(self):
        self.always_on_top = not self.always_on_top
        # setWindowFlag 会重建原生窗口并隐藏它，重新显示时需保留最大化状态
        self.setWindowFlag(Qt.WindowStaysOnTopHint, self.always_on_top)
        self._show_preserving_state()

        if self.always_on_top:
            self.always_on_top_button.setText("取消")
            self._update_toggle_button_style()
        else:
            self.always_on_top_button.setText("置顶")
            self.always_on_top_button.setStyleSheet("")

    def _sync_lang_combo_widths(self):
        """两个语言下拉框取相同宽度（按较长的选中文字 + 左右内边距，
        保底 80px），使转换符号两侧的框体对称、间距视觉均匀。"""
        combos = (self.source_lang_combo, self.target_lang_combo)
        width = max(
            max(c.fontMetrics().horizontalAdvance(c.currentText()) + 40, 80)
            for c in combos
        )
        for combo in combos:
            combo.setFixedWidth(width)

    def on_lang_selection_changed(self, selection):
        self.save_selections()

    def start_ocr_and_translate(self):
        """Queue an OCR request with auto-translation enabled."""
        self._prepare_translation_hot_path()
        self._dispatch_ocr_request(auto_translate=True, show_window=True)

    def threaded_start_ocr(self):
        """Hide the window, then queue OCR on the next event loop turn."""
        self._hide_and_request_ocr(auto_translate=False, show_window=True)

    def _reset_splitter_sizes(self):
        """恢复识别区/翻译区的默认等高布局。

        按 splitter 实际高度动态计算，窗口尺寸调整后无需手工重算：
        总高扣除手柄与下半面板多出的语言栏部分后对半分。"""
        total = self.splitter.height() - self.splitter.handleWidth()
        if total <= 0:
            return  # 几何未就绪（如 --silent 启动未显示过），留待 showEvent 处理
        box = (total - self._splitter_bottom_extra) // 2
        self.splitter.setSizes([box, total - box])

    def showEvent(self, event):
        super().showEvent(event)
        # 首次显示时 splitter 才有有效几何，在此应用默认等高布局；
        # 之后的显示不再重置，保留用户拖动的分割位置
        if not self._splitter_equalized:
            self._splitter_equalized = True
            self._reset_splitter_sizes()

    def _dispatch_hotkey_ocr_request(self, auto_translate, show_window):
        """热键触发的 OCR 请求入口（ocr_request_signal 仅由热键路径发射）。

        热键启动时先把分割条恢复为默认等高布局再分发请求；
        按钮触发的识别直接调用 _dispatch_ocr_request，不受影响。
        """
        self._reset_splitter_sizes()
        return self._dispatch_ocr_request(auto_translate, show_window)

    def _dispatch_ocr_request(self, auto_translate, show_window):
        """Centralized OCR request entrypoint with busy-state protection."""
        trace_label = None
        if auto_translate and self._tracer.is_active:
            trace_label = self._tracer.active_id
            self._tracer.log("dispatching OCR request")

        queued = self._ocr_mgr.request_ocr(
            auto_translate=auto_translate,
            show_window=show_window,
            trace_label=trace_label
        )
        if not queued:
            logging.debug(
                "OCR request ignored because another request is already in progress "
                f"(auto_translate={auto_translate}, show_window={show_window})"
            )
            if trace_label:
                self._tracer.finish("aborted", "OCR busy")
        return queued

    def _hide_and_request_ocr(self, auto_translate, show_window):
        """隐藏主窗口，待其从屏幕上彻底消失后再请求 OCR。

        QWidget.hide() 在 Win32 层是同步的，但桌面窗口管理器（DWM）默认会对窗口隐藏
        施加约 150–250ms 的淡出动画，动画期间窗口仍残留在合成帧缓冲中；而截图走的是
        mss 全屏抓取（直接读取合成后的帧缓冲），因此在动画结束前抓屏会拍到正在淡出的
        主窗口，挡住待识别内容。

        根本解决：① 在窗口创建时已禁用 DWM 过渡动画，使隐藏瞬时完成；
        ② 这里再以“对窗口原区域逐帧采样直至画面稳定”的方式校验窗口确已消失，
        对任意合成/动画/GPU 时序都稳健。
        """
        # 抓屏前记录窗口当前屏幕矩形，用于校验其是否已从画面中消失
        window_rect = None
        try:
            if self.isVisible():
                window_rect = win32gui.GetWindowRect(int(self.winId()))
        except Exception:
            window_rect = None

        self.hide()
        self._wait_until_window_off_screen(window_rect)
        self._dispatch_ocr_request(
            auto_translate=auto_translate,
            show_window=show_window
        )

    def _wait_until_window_off_screen(self, window_rect=None, timeout_ms=600):
        """阻塞直到主窗口从屏幕帧缓冲中真正消失后再返回（截图前调用）。

        采用“对窗口原区域逐帧采样直至画面稳定”的校验式等待：隐藏淡出动画期间该区域
        像素逐帧变化，动画结束、窗口彻底消失后画面趋于稳定；据此可靠判定可以安全抓屏。
        无动画时通常一两帧（~16–48ms）即返回，最长不超过 timeout_ms，避免固定延迟
        “要么太短挡住内容、要么太长拖慢响应”的两难。
        """
        # 刷新 Qt 事件，确保 hide 已提交到平台层
        try:
            QApplication.processEvents()
        except Exception:
            pass

        def _flush():
            """阻塞到下一次 DWM 合成完成；不可用时返回 False。"""
            try:
                ctypes.windll.dwmapi.DwmFlush()
                return True
            except Exception:
                return False

        # 无法获知窗口区域时，退化为合成器刷新 + 固定延迟兜底
        if not window_rect:
            if not (_flush() and _flush()):
                time.sleep(0.2)
            return

        left, top, right, bottom = window_rect
        region = {
            'left': int(left), 'top': int(top),
            'width': max(1, int(right - left)), 'height': max(1, int(bottom - top)),
        }

        try:
            import mss as _mss
            deadline = time.perf_counter() + timeout_ms / 1000.0
            prev = None
            stable = 0
            with _mss.mss() as sct:
                # 先等待一次合成，确保比较的是 hide 之后的新帧而非隐藏前的旧帧
                if not _flush():
                    time.sleep(0.016)
                while time.perf_counter() < deadline:
                    cur = sct.grab(region).rgb
                    if prev is not None and cur == prev:
                        stable += 1
                        if stable >= 2:  # 连续 3 帧一致 → 动画结束、窗口已消失
                            return
                    else:
                        stable = 0
                    prev = cur
                    if not _flush():
                        time.sleep(0.016)
        except Exception as e:
            logging.debug(f"窗口隐藏校验失败，回退到固定延迟: {e}")
            time.sleep(0.2)

    def _disable_window_transitions(self):
        """禁用主窗口的 DWM 过渡动画，使 hide()/show() 瞬时生效（无淡入淡出）。

        消除“识别”截图时因隐藏淡出动画导致主窗口残留在画面中的根因；同时让隐藏校验
        在常见情况下一两帧即可返回。仅 Windows 有效，失败不影响功能。
        """
        try:
            hwnd = int(self.winId())
            DWMWA_TRANSITIONS_FORCEDISABLED = 3
            value = ctypes.c_int(1)
            dwm = ctypes.windll.dwmapi
            dwm.DwmSetWindowAttribute.argtypes = [
                wintypes.HWND, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD
            ]
            dwm.DwmSetWindowAttribute(
                wintypes.HWND(hwnd),
                DWMWA_TRANSITIONS_FORCEDISABLED,
                ctypes.byref(value),
                ctypes.sizeof(value),
            )
        except Exception as e:
            logging.debug(f"禁用窗口过渡动画失败: {e}")

    def trigger_hotkey_ocr(self):
        """Dispatch the OCR hotkey request back onto the Qt main thread."""
        self.ocr_request_signal.emit(False, False)

    def trigger_hotkey_translate(self):
        """Dispatch the OCR+translate hotkey request back onto the Qt main thread."""
        self.hotkey_translate_signal.emit()

    def _handle_hotkey_translate(self):
        """Run the OCR+translate hotkey flow on the Qt main thread."""
        if self._is_shutting_down:
            return
        trace = self._tracer.start("hotkey")
        self._prepare_translation_hot_path(trace['id'])
        self.ocr_request_signal.emit(True, True)

    def _prepare_translation_hot_path(self, trace_label=None):
        """Prewarm Bing token immediately so OCR time can overlap translation preparation."""
        try:
            if trace_label:
                self._tracer.log("requested Bing token warm-up")
            self._trans_mgr.prepare_hotkey_translation(trace_label=trace_label)
        except Exception as e:
            logging.debug(f"Prepare translation hot path failed: {e}")

    def display_ocr_result_and_translate(self, text, auto_translate, show_window):
        # 空值检查，防止 None 导致崩溃
        if text is None:
            text = ""

        # 在方法内部处理 show_window 逻辑
        if show_window:
            self.show()  # 显示主窗口
            self.activateWindow()  # 激活主窗口

        self.result_box.clear()
        self.result_box.setText(text)

        # 只有非空文本才复制到剪贴板
        if text:
            try:
                QApplication.clipboard().setText(text)
            except Exception as e:
                logging.warning(f"复制到剪贴板失败: {e}")

        if auto_translate:
            if self._tracer.is_active:
                self._tracer.log(f"OCR result received by UI; chars={len(text)}")
            # 切回事件循环一轮，避免阻塞当前 UI 更新
            QTimer.singleShot(0, self.translate_text)
        else:
            self.translate_box.clear()
            # F4 静默识别（show_window=False）不弹出主窗口，不会经过 closeEvent
            # 的工作集回收路径；在此延迟回收，避免连续后台识别时截图缓冲触碰的
            # 物理页持续累积在工作集中（表现为任务管理器内存只增不减）
            QTimer.singleShot(2000, self._trim_if_backgrounded)

    def translate_text(self):
        text = self.result_box.toPlainText().strip()
        if not text:
            self.error_signal.emit("翻译提示", "没有可翻译的文本。")
            self.translate_box.clear()  # 清空翻译框，避免显示旧结果
            return

        # 在翻译窗口显示提示信息
        self.translate_box.setText("请稍等翻译中...")

        # 获取语言参数
        source_lang_text = self.source_lang_combo.currentText()
        target_lang_text = self.target_lang_combo.currentText()
        from_lang = self.lang_codes.get(source_lang_text, 'auto')
        to_lang = self.lang_codes.get(target_lang_text, 'zh-Hans')
        trace_label = self._tracer.active_id
        if trace_label:
            self._tracer.log(f"dispatching translation; from={from_lang} to={to_lang}")

        # 使用 TranslationManager
        self._trans_mgr.translate(text, from_lang, to_lang, trace_label=trace_label)

    def on_translation_done(self, translated_text):
        """在主线程中安全地更新翻译结果。"""
        self.translate_box.setText(translated_text)
        try:
            QApplication.clipboard().setText(translated_text)
        except Exception as e:
            logging.warning(f"复制翻译结果到剪贴板失败: {e}")
        self._tracer.finish("completed", f"translated_chars={len(translated_text)}")

    def show_error_message(self, title, message):
        logging.warning(f"[{title}] {message}")
        self._tracer.maybe_finish_on_error(title, message)
        # 后台识别被取消（Esc）或失败时同样不经过 closeEvent，
        # 此处兜底回收一次工作集（窗口可见时自动跳过）
        QTimer.singleShot(2000, self._trim_if_backgrounded)

    def show_custom_message(self, title, message, icon_type=QMessageBox.Information):
        logging.info(f"[{title}] {message}")

    def save_settings(self, settings_dict):
        """Save settings using ConfigManager."""
        # Update ConfigManager
        self._config.update({
            'api_key': settings_dict.get('api_key', ''),
            'secret_key': settings_dict.get('secret_key', ''),
            'baidu_trans_appid': settings_dict.get('baidu_trans_appid', ''),
            'baidu_trans_appkey': settings_dict.get('baidu_trans_appkey', ''),
            'tencent_secret_id': settings_dict.get('tencent_secret_id', ''),
            'tencent_secret_key': settings_dict.get('tencent_secret_key', ''),
            'tencent_trans_secret_id': settings_dict.get('tencent_trans_secret_id', ''),
            'tencent_trans_secret_key': settings_dict.get('tencent_trans_secret_key', ''),
            'hotkey': settings_dict.get('hotkey', 'F4'),
            'translate_hotkey': settings_dict.get('translate_hotkey', 'F2'),
            'hide_tray_icon': settings_dict.get('hide_tray_icon', False),
            'replace_newline': settings_dict.get('replace_newline', False),
        })

        logging.debug("Saved settings")

        # Update instance variables
        self.api_key_var = self._config.api_key
        self.secret_key_var = self._config.secret_key
        self.baidu_trans_appid_var = self._config.baidu_trans_appid
        self.baidu_trans_appkey_var = self._config.baidu_trans_appkey
        self.tencent_secret_id_var = self._config.tencent_secret_id
        self.tencent_secret_key_var = self._config.tencent_secret_key
        self.tencent_trans_secret_id_var = self._config.tencent_trans_secret_id
        self.tencent_trans_secret_key_var = self._config.tencent_trans_secret_key
        self.hotkey_var = self._config.hotkey
        self.translate_hotkey_var = self._config.translate_hotkey
        self.hide_tray_icon_var = self._config.hide_tray_icon
        self.replace_newline_var = self._config.replace_newline
        self.last_ocr_selection = self._config.last_ocr_selection
        self.last_translate_selection = self._config.last_translate_selection

        # 记录日志，帮助调试
        logging.debug(f"Loaded OCR selection: {self.last_ocr_selection}")
        logging.debug(f"Loaded translate selection: {self.last_translate_selection}")

        # 立即更新热键
        logging.debug(f"Attempting to update hotkeys. OCR: {self.hotkey_var}, Translate: {self.translate_hotkey_var}")
        if not self._is_shutting_down and hasattr(self, 'hotkey_handler') and self.hotkey_handler:
            try:
                self.hotkey_handler.update_ocr_hotkey(self.hotkey_var, self.trigger_hotkey_ocr)
                self.hotkey_handler.update_translate_hotkey(self.translate_hotkey_var, self.trigger_hotkey_translate)
            except Exception as e:
                logging.error(f"更新热键失败: {e}")
                self.show_custom_message("提示", f"更新热键失败: {e}\n请尝试使用不同的热键组合。", QMessageBox.Information)

        # 更新托盘菜单快捷键标签
        self.update_tray_hotkey_labels()

        # 更新其他可能受影响的组件
        self.update_ui_components()

    def init_hotkey_handler(self):
        from src.hotkey_handler import HotkeyHandler
        try:
            self.hotkey_handler = HotkeyHandler()
            if not self.hotkey_handler.start_hotkey_listener():
                raise RuntimeError("热键监听线程启动失败")
            
            logging.info(f"尝试注册OCR热键: {self.hotkey_var}")
            self.hotkey_handler.update_ocr_hotkey(self.hotkey_var, self.trigger_hotkey_ocr)
            
            logging.info(f"尝试注册翻译热键: {self.translate_hotkey_var}")
            self.hotkey_handler.update_translate_hotkey(self.translate_hotkey_var, self.trigger_hotkey_translate)

        except Exception as e:
            logging.exception("热键注册失败")
            error_message = f"热键注册失败: {str(e)}\n\n请重新设置热键。"
            logging.error(error_message)
            self.show_custom_message("提示", error_message, QMessageBox.Information)

    def update_hotkey(self, hotkey_var, translate_hotkey_var, auto_update_var):
        try:
            self.hotkey_handler.update_ocr_hotkey(hotkey_var, self.trigger_hotkey_ocr)
            self.hotkey_handler.update_translate_hotkey(translate_hotkey_var, self.trigger_hotkey_translate)
            logging.info(f"OCR热键更新为: {hotkey_var}")
            logging.info(f"翻译热键更新为: {translate_hotkey_var}")
            self.hotkey_var = hotkey_var
            self.translate_hotkey_var = translate_hotkey_var
            
        except Exception as e:
            logging.exception("热键更新失败")
            error_message = f"热键更新失败: {str(e)}\n\n请尝试使用不同的热键组合。"
            logging.error(error_message)
            self.show_custom_message("提示", error_message, QMessageBox.Information)

    def open_settings_window(self):
        from src.settings_window import SettingsWindow
        try:
            # M2: 复用已打开的设置窗口。重复点击「设置」时把现有窗口前置，
            # 而不是新建实例（旧实例此前需等 GC 才回收，多轮开关会短时驻留多个）
            existing = getattr(self, 'settings_window_ref', None)
            if existing is not None:
                try:
                    if existing.isVisible():
                        existing.raise_()
                        existing.activateWindow()
                        return
                except RuntimeError:
                    # C++ 对象已随 WA_DeleteOnClose 销毁，丢弃悬挂引用后重建
                    self.settings_window_ref = None

            # Reload config from ConfigManager
            self._config.reload()
            self.api_key_var = self._config.api_key
            self.secret_key_var = self._config.secret_key
            self.baidu_trans_appid_var = self._config.baidu_trans_appid
            self.baidu_trans_appkey_var = self._config.baidu_trans_appkey
            self.tencent_secret_id_var = self._config.tencent_secret_id
            self.tencent_secret_key_var = self._config.tencent_secret_key
            self.tencent_trans_secret_id_var = self._config.tencent_trans_secret_id
            self.tencent_trans_secret_key_var = self._config.tencent_trans_secret_key
            self.hotkey_var = self._config.hotkey
            self.translate_hotkey_var = self._config.translate_hotkey
            self.replace_newline_var = self._config.replace_newline
            self.auto_update_var = self._config.get('auto_update', False)

            settings_window = SettingsWindow(
                self,
                self.api_key_var,
                self.secret_key_var,
                self.hotkey_var,
                self.translate_hotkey_var,
                self.update_hotkey,
                self.save_settings,
                self.baidu_trans_appid_var,
                self.baidu_trans_appkey_var,
                self.hide_tray_icon_var,
                self.tencent_secret_id_var,
                self.tencent_secret_key_var,
                self.tencent_trans_secret_id_var,
                self.tencent_trans_secret_key_var,
                self.replace_newline_var
            )
            
            # 关闭即销毁 C++ 对象，避免实例随多轮开关累积驻留(M2)
            settings_window.setAttribute(Qt.WA_DeleteOnClose)

            # 调整窗口位置使其在父窗口中居中
            settings_window.move_to_center()

            # 使用show()方法代替exec()
            # settings_window.exec()
            settings_window.show()

            # 保持对设置窗口的引用，防止被垃圾回收
            self.settings_window_ref = settings_window
            
            logging.debug("设置窗口已显示")
        except Exception as e:
            logging.exception("打开设置窗口失败")
            error_message = f"打开设置窗口失败: {str(e)}"
            self.show_custom_message("错误", error_message, QMessageBox.Information)

    def init_session_monitor(self):
        from src.session_monitor import SessionMonitor
        self.session_monitor = SessionMonitor(self.hotkey_handler)
        self.session_monitor.start_session_monitor_thread()

    def _start_background_health_monitor(self):
        """定期检查后台线程，防止长时间托盘驻留后静默失效。"""
        self._background_health_timer = QTimer(self)
        self._background_health_timer.setInterval(120000)
        self._background_health_timer.timeout.connect(self._check_background_services)
        self._background_health_timer.start()

    def _check_background_services(self):
        """恢复可能在长时间后台驻留后失效的后台组件。"""
        if self._is_shutting_down:
            return

        if hasattr(self, 'hotkey_handler') and self.hotkey_handler and not self.hotkey_handler.is_listener_alive():
            logging.warning("检测到热键监听器失效，正在尝试恢复")
            if self.hotkey_handler.start_hotkey_listener():
                self.hotkey_handler.re_register_hotkeys()

        if hasattr(self, 'session_monitor') and self.session_monitor and not self.session_monitor.is_alive():
            logging.warning("检测到会话监控器失效，正在尝试恢复")
            self.session_monitor.start_session_monitor_thread()

    def init_tray_icon(self):
        """Initialize the system tray icon using TrayManager."""
        self._tray_mgr = TrayManager(self)
        self._tray_mgr.set_callbacks(
            on_show=self._safe_show_window,
            on_ocr=self.threaded_start_ocr,
            on_translate=self.start_ocr_and_translate,
            on_settings=self.open_settings_window,
            on_restart=self.restart_program,
            on_quit=self.quit_application
        )
        self._tray_mgr.update_hotkey_labels(self.hotkey_var, self.translate_hotkey_var)

        # 根据配置决定是否显示托盘图标
        self._tray_mgr.set_visible(not self.hide_tray_icon_var)

    def _safe_show_window(self):
        """Safely show and activate the main window (used by tray callbacks)."""
        try:
            self._show_preserving_state()
            self.activateWindow()
        except RuntimeError:
            logging.debug("MainWindow C++ object deleted during _safe_show_window")

    def update_tray_icon_visibility(self, hide):
        logging.debug(f"update_tray_icon_visibility called with hide={hide}")
        if hasattr(self, '_tray_mgr'):
            self._tray_mgr.set_visible(not hide)

    def update_tray_hotkey_labels(self):
        """更新托盘菜单中的快捷键标签"""
        if hasattr(self, '_tray_mgr'):
            self._tray_mgr.update_hotkey_labels(self.hotkey_var, self.translate_hotkey_var)

    def quit_application(self):
        self._is_shutting_down = True

        if hasattr(self, '_background_health_timer'):
            self._background_health_timer.stop()

        # 进行必要的清理操作
        if hasattr(self, '_tray_mgr'):
            self._tray_mgr.cleanup()

        # Stop managers
        if hasattr(self, '_ocr_mgr'):
            self._ocr_mgr.stop()

        # 停止翻译保活线程
        if hasattr(self, '_trans_mgr'):
            try:
                self._trans_mgr.stop_keepalive()
            except Exception as e:
                logging.debug(f"停止翻译保活时出错: {e}")

        # 停止主题监控定时器
        if hasattr(self, '_theme_mgr'):
            try:
                self._theme_mgr.stop_monitoring()
            except Exception as e:
                logging.debug(f"停止主题监控时出错: {e}")

        # 停止截图清理线程
        try:
            from src.screenshot import stop_cleanup_thread
            stop_cleanup_thread()
        except Exception as e:
            logging.debug(f"停止清理线程时出错: {e}")

        # 停止会话监控
        if hasattr(self, 'session_monitor'):
            try:
                self.session_monitor.stop()
            except Exception as e:
                logging.debug(f"停止会话监控时出错: {e}")

        # 停止热键监听
        if hasattr(self, 'hotkey_handler'):
            try:
                self.hotkey_handler.stop_listener()
            except Exception as e:
                logging.debug(f"停止热键监听时出错: {e}")

        # 清理敏感的令牌缓存和 HTTP 会话
        try:
            from src.ocr import clear_sensitive_cache as clear_ocr_cache
            from src.translate import clear_sensitive_cache as clear_translate_cache
            from src.tencent_utils import close_tencent_session
            clear_ocr_cache()
            clear_translate_cache()
            close_tencent_session()
        except Exception as e:
            logging.warning(f"清理缓存时出错: {e}")

        _release_single_instance()
        QApplication.instance().quit()

    def restart_program(self):
        try:
            logging.info("正在执行重启程序...")
            self._is_shutting_down = True

            if hasattr(self, '_background_health_timer'):
                self._background_health_timer.stop()

            # 1. 保存当前设置
            self.save_selections()
            settings_to_save = {
                'api_key': self.api_key_var,
                'secret_key': self.secret_key_var,
                'baidu_trans_appid': self.baidu_trans_appid_var,
                'baidu_trans_appkey': self.baidu_trans_appkey_var,
                'tencent_secret_id': self.tencent_secret_id_var,
                'tencent_secret_key': self.tencent_secret_key_var,
                'tencent_trans_secret_id': self.tencent_trans_secret_id_var,
                'tencent_trans_secret_key': self.tencent_trans_secret_key_var,
                'hotkey': self.hotkey_var,
                'translate_hotkey': self.translate_hotkey_var,
                'hide_tray_icon': self.hide_tray_icon_var,
                'replace_newline': self.replace_newline_var
            }
            self.save_settings(settings_to_save)

            # 2. 隐藏托盘图标，防止残留 ghost icon
            if hasattr(self, '_tray_mgr'):
                self._tray_mgr.hide()

            # 3. 确保文件写入
            time.sleep(0.1)

            # 4. 释放单实例互斥锁，允许新进程获取
            _release_single_instance()

            # 5. 启动新进程
            import subprocess
            if getattr(sys, 'frozen', False):
                app_path = sys.executable
                subprocess.Popen([app_path])
            else:
                script_path = os.path.abspath(sys.argv[0])
                subprocess.Popen([sys.executable, script_path])

            # 6. 退出当前进程
            QApplication.quit()

        except Exception as e:
            logging.error(f"重启程序时出错: {e}")
            # 尝试重新获取互斥锁（如果已释放但启动新进程失败）
            if _instance_mutex is None:
                _try_acquire_single_instance()
            self._is_shutting_down = False
            if hasattr(self, '_tray_mgr'):
                self._tray_mgr.show()
            if hasattr(self, '_background_health_timer'):
                self._background_health_timer.start()
            self.show_custom_message("重启错误", f"重启程序时出错: {e}", QMessageBox.Information)

    def update_ui_components(self):
        # 更新可能受到设置变化影响的 UI 组件
        # 移除强制设置OCR接口的代码，改为检查并提示
        # 更新其他可能受影响的 UI 组件
        pass

    def load_last_selections(self):
        # 设置上次选择的OCR和翻译接口
        original_ocr_selection = self.last_ocr_selection
        if self.last_ocr_selection == "默认":
            self.default_text_action.setChecked(True)
        elif self.last_ocr_selection == "默认_table":
            self.default_table_action.setChecked(True)
        elif self.last_ocr_selection.startswith("Baidu_"):
            # If the saved selection is a specific one that has been removed, map it to "Baidu_auto"
            if self.last_ocr_selection not in self.baidu_actions:
                self.last_ocr_selection = "Baidu_auto"

            action_to_check = self.baidu_actions.get(self.last_ocr_selection)
            if action_to_check:
                action_to_check.setChecked(True)
            else: # Safeguard, default to "默认"
                self.default_text_action.setChecked(True)
                self.last_ocr_selection = "默认"
        elif self.last_ocr_selection.startswith("Tencent_"):
            # Map old selections to the new 'Tencent_auto' for UI checking
            if self.last_ocr_selection in ["Tencent_general_basic", "Tencent_general_accurate"]:
                self.last_ocr_selection = "Tencent_auto"

            action_to_check = self.tencent_actions.get(self.last_ocr_selection)
            if action_to_check:
                action_to_check.setChecked(True)
            else: # Safeguard, default to "默认"
                self.default_text_action.setChecked(True)
                self.last_ocr_selection = "默认"
        else:
            # Default to "默认" if selection is completely unknown
            self.default_text_action.setChecked(True)
            self.last_ocr_selection = "默认"

        # 若旧选项被映射为新值，持久化回配置，避免内存与磁盘不一致
        if self.last_ocr_selection != original_ocr_selection:
            self._config.last_ocr_selection = self.last_ocr_selection
            self._config.save()
            logging.debug(
                f"OCR 选项已从 '{original_ocr_selection}' 迁移为 '{self.last_ocr_selection}' 并保存"
            )

        if self.last_translate_selection == "默认":
            self.bing_action.setChecked(True)
        elif self.last_translate_selection == "Baidu":
            self.baidu_trans_action.setChecked(True)
        elif self.last_translate_selection == "Tencent":
            self.tencent_trans_action.setChecked(True)

        self.source_lang_combo.setCurrentText(self.last_source_lang)
        self.target_lang_combo.setCurrentText(self.last_target_lang)

    def save_selections(self):
        """保存当前选择的OCR和翻译接口"""
        self._config.last_ocr_selection = self.last_ocr_selection
        self._config.last_translate_selection = self.last_translate_selection

        source_lang_text = self.source_lang_combo.currentText()
        if source_lang_text:
            self._config.set('source_lang', self.lang_codes[source_lang_text], auto_save=False)
            self._config.last_source_lang = source_lang_text

        target_lang_text = self.target_lang_combo.currentText()
        if target_lang_text:
            self._config.set('target_lang', self.lang_codes[target_lang_text], auto_save=False)
            self._config.last_target_lang = target_lang_text

        self._config.save()

    def on_ocr_selection_changed(self, selection):
        """更新当前OCR选择并保存"""
        self.last_ocr_selection = selection
        self._config.last_ocr_selection = selection
        self._config.save()
        # 切换接口后立即在后台预热所需的重量级资源（OneOCR 引擎/表格识别依赖），
        # 保证用户随后的首次识别没有冷启动延迟
        if hasattr(self, '_ocr_mgr'):
            self._ocr_mgr.preload_for_selection(selection)
        logging.debug(f"OCR接口已切换为: {selection}")

    def update_ocr_selection_ui(self, selection_id):
        """Updates the checkmark in the OCR source menu on the main thread."""
        if selection_id in self.baidu_actions:
            action = self.baidu_actions.get(selection_id)
            if action:
                action.setChecked(True)
        elif selection_id in self.tencent_actions:
            action = self.tencent_actions.get(selection_id)
            if action:
                action.setChecked(True)
        elif selection_id == "默认":
            self.default_text_action.setChecked(True)
        elif selection_id == "默认_table":
            self.default_table_action.setChecked(True)
        logging.debug(f"OCR selection UI updated to '{selection_id}'.")

    def update_translation_selection_ui(self, selection_id):
        """Updates the checkmark in the translation source menu on the main thread."""
        if selection_id == "Baidu":
            self.baidu_trans_action.setChecked(True)
        elif selection_id == "Tencent":
            self.tencent_trans_action.setChecked(True)
        elif selection_id == "默认":
            self.bing_action.setChecked(True)
        logging.debug(f"Translation selection UI updated to '{selection_id}'.")

    def on_translate_selection_changed(self, selection):
        """更新当前翻译选择并保存"""
        self.last_translate_selection = selection
        self._config.last_translate_selection = selection
        self._config.save()

        # 检查翻译源的配置
        if selection == "Baidu" and not self._config.has_baidu_trans_credentials():
            self.show_custom_message("警告", "未配置百度翻译API，请在设置中配置。", QMessageBox.Information)

        logging.debug(f"翻译源已切换为: {selection}")

class TinyNotification(QWidget):
    def __init__(self, title: str, message: str, parent=None):
        super().__init__(
            parent,
            Qt.ToolTip | Qt.FramelessWindowHint | Qt.WindowStaysOnTopHint
        )
        self.setAttribute(Qt.WA_ShowWithoutActivating)
        self.setAttribute(Qt.WA_TranslucentBackground)

        card = QFrame(self)
        card.setObjectName("card")
        card.setStyleSheet("""
            QFrame#card {
                background-color: #ffffff;
                border: 1px solid #dcdcdc;
                border-radius: 8px;
            }
            QLabel#title {
                color: #222;
                font-size: 12px;
            }
            QLabel#msg {
                color: #222;
                font-size: 12px;
            }
        """)

        card_layout = QVBoxLayout(card)
        card_layout.setContentsMargins(12, 8, 12, 8)
        card_layout.setSpacing(4)

        if title:
            title_label = QLabel(title)
            title_label.setObjectName("title")
            card_layout.addWidget(title_label)

        msg_label = QLabel(message)
        msg_label.setObjectName("msg")
        msg_label.setWordWrap(True)
        card_layout.addWidget(msg_label)

        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.addWidget(card)

        self.adjustSize()

        # ===== 定位到右下角 =====
        screen = QApplication.primaryScreen().availableGeometry()
        self.move(
            screen.right() - self.width() - 20,
            screen.bottom() - self.height() - 20
        )

        # ===== 自动关闭 =====
        QTimer.singleShot(5000, self.close)

    def show_tiny(self):
        self.show()

def main():
    try:
        multiprocessing.freeze_support()
        if not run_as_admin():
            sys.exit(0)

        # 单实例检测（使用内核级 Named Mutex，原子操作无竞态）
        if not _try_acquire_single_instance():
            # 已有实例运行，尝试激活其窗口后退出
            _activate_existing_instance()
            sys.exit(0)

        app = QApplication(sys.argv)
        disable_dpi_scaling()
        app.setQuitOnLastWindowClosed(False)

        # 1. 先显示主界面（确保启动极快）
        main_window = MainWindow()
        if '--silent' not in sys.argv:
            main_window.show()

        # 2. 定义延迟更新检查函数
        def delayed_update_check(main_win):
            class UpdateWorker(QThread):
                finished_sig = Signal(bool)
                def run(self):
                    from src.auto_update import perform_daily_update_check
                    # 后台静默检查
                    res = perform_daily_update_check()
                    self.finished_sig.emit(res)

            worker = UpdateWorker(main_win)
    
            def handle_result(is_updating):
                if is_updating:
                    # 采用自定义小窗口，取消蓝色感
                    notifier = TinyNotification("SightOCR", "新版本已就绪", main_win)
                    notifier.show_tiny()
                    main_win._notifier = notifier # 保持引用

            worker.finished_sig.connect(handle_result)
            worker.start()
            main_win._update_worker = worker

        # 3. 核心修改：启动 3 分钟 后执行更新检查，完全不占用启动时间
        QTimer.singleShot(180000, lambda: delayed_update_check(main_window)) 

        sys.exit(app.exec())

    except Exception as e:
        logging.critical(f"程序运行时发生致命错误：{e}")
        import traceback
        logging.critical(traceback.format_exc())
        sys.exit(1)

if __name__ == "__main__":
    main()
