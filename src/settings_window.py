# settings_window.py

from PySide6.QtWidgets import (
    QDialog, QLabel, QLineEdit, QPushButton,
    QMessageBox, QGridLayout, QVBoxLayout, QHBoxLayout,
    QWidget, QStackedWidget, QFrame, QListWidget,
    QListWidgetItem
)
from PySide6.QtGui import QIcon, QKeySequence
from PySide6.QtCore import Qt, QEvent, Signal, QTimer, QSize
from .utils import resource_path
from .autostart import set_autostart, is_autostart_enabled
from .managers.theme_manager import ThemeManager
import webbrowser
import logging
from .auto_update import CURRENT_VERSION


class ApiConfigDialog(QDialog):
    """通用 API 配置子窗口"""
    config_saved = Signal(dict)

    def __init__(self, parent, title, fields, links=None):
        super().__init__(parent)
        self.setWindowTitle(title)
        self.setWindowIcon(QIcon(resource_path("assets/icon.png")))
        self.setFixedWidth(420)
        self.setModal(True)

        self.fields = fields
        self.links = links or []
        self.field_edits = {}
        self._theme_mgr = ThemeManager.instance()

        self._apply_style()
        self.init_ui()
        self._connect_theme_signal()

    def _connect_theme_signal(self):
        """Connect to theme change signal."""
        try:
            self._theme_mgr.theme_changed.connect(
                self._on_theme_changed,
                Qt.UniqueConnection
            )
        except TypeError:
            pass

    def _disconnect_theme_signal(self):
        """Disconnect theme change signal."""
        try:
            self._theme_mgr.theme_changed.disconnect(self._on_theme_changed)
        except (TypeError, RuntimeError):
            pass

    def closeEvent(self, event):
        """断开信号连接"""
        self._disconnect_theme_signal()
        super().closeEvent(event)

    def _on_theme_changed(self, is_dark: bool):
        """Handle theme change."""
        self._apply_style()

    def _apply_style(self):
        colors = self._theme_mgr.get_colors()
        self.setStyleSheet(f"""
            QDialog {{
                background-color: {colors['bg']};
            }}
            QLabel {{
                color: {colors['text']};
                font-size: 14px;
                background: transparent;
            }}
            QLineEdit {{
                background-color: {colors['content_bg']};
                border: 1px solid {colors['border']};
                border-radius: 6px;
                padding: 8px 12px;
                font-size: 13px;
                color: {colors['text']};
            }}
            QLineEdit:focus {{
                border: 1px solid {colors['primary']};
            }}
            QPushButton {{
                background-color: {colors['content_bg']};
                border: 1px solid {colors['border']};
                border-radius: 6px;
                padding: 8px 16px;
                font-size: 13px;
                color: {colors['text']};
            }}
            QPushButton:hover {{
                background-color: {colors['hover_bg']};
            }}
        """)

    def init_ui(self):
        layout = QVBoxLayout()
        self.setLayout(layout)
        layout.setContentsMargins(20, 20, 20, 20)
        layout.setSpacing(18)

        for field in self.fields:
            field_layout = QVBoxLayout()
            field_layout.setSpacing(6)

            label = QLabel(field['label'])
            field_layout.addWidget(label)

            edit = MaskedLineEdit(field.get('value', ''))
            edit.setPlaceholderText(field.get('placeholder', ''))
            edit.setFixedHeight(36)
            self.field_edits[field['key']] = edit
            field_layout.addWidget(edit)

            layout.addLayout(field_layout)

        btn_layout = QHBoxLayout()
        btn_layout.setSpacing(12)

        if self.links:
            get_key_btn = QPushButton(self.links[0]['label'])
            get_key_btn.setFixedSize(90, 34)
            get_key_url = self.links[0]['url']
            get_key_btn.clicked.connect(lambda: webbrowser.open(get_key_url))
            btn_layout.addWidget(get_key_btn)

        btn_layout.addStretch()

        save_btn = QPushButton("保存")
        save_btn.setFixedSize(90, 34)
        save_btn.clicked.connect(self.save_config)
        btn_layout.addWidget(save_btn)

        layout.addLayout(btn_layout)

    def showEvent(self, event):
        """显示时居中到父窗口"""
        super().showEvent(event)
        self._move_to_center()

    def _move_to_center(self):
        """将窗口移动到父窗口的中央"""
        if self.parent():
            parent_rect = self.parent().geometry()
            self.move(
                parent_rect.x() + (parent_rect.width() - self.width()) // 2,
                parent_rect.y() + (parent_rect.height() - self.height()) // 2
            )

    def save_config(self):
        result = {}
        for key, edit in self.field_edits.items():
            result[key] = edit.real_text
        self.config_saved.emit(result)
        self.accept()

    def get_values(self):
        return {key: edit.real_text for key, edit in self.field_edits.items()}


class SettingsWindow(QDialog):
    def __init__(self, parent, api_key_var,
             secret_key_var, hotkey_var, translate_hotkey_var, update_hotkey, save_settings,
             baidu_trans_appid_var, baidu_trans_appkey_var, hide_tray_icon_var,
             tencent_secret_id_var='', tencent_secret_key_var='',
             tencent_trans_secret_id_var='', tencent_trans_secret_key_var='',
             replace_newline_var=False):
        super().__init__(parent)
        self.setWindowTitle("设置")
        self.setWindowIcon(QIcon(resource_path("assets/icon.png")))
        # 固定窗口尺寸（与主窗口设计一致）：PySide6 6.8+ 不再因
        # setFixedSize 自动移除 WS_MAXIMIZEBOX，需显式设置
        # MSWindowsFixedSizeDialogHint，否则打包环境下仍可调整大小/最大化
        self.setFixedSize(480, 360)
        self.setWindowFlag(Qt.MSWindowsFixedSizeDialogHint, True)
        self.setModal(True)

        self.api_key_var = api_key_var
        self.secret_key_var = secret_key_var
        self.baidu_trans_appid_var = baidu_trans_appid_var
        self.baidu_trans_appkey_var = baidu_trans_appkey_var
        self.tencent_secret_id_var = tencent_secret_id_var
        self.tencent_secret_key_var = tencent_secret_key_var
        self.tencent_trans_secret_id_var = tencent_trans_secret_id_var
        self.tencent_trans_secret_key_var = tencent_trans_secret_key_var
        self.hotkey_var = hotkey_var
        self.translate_hotkey_var = translate_hotkey_var
        self.update_hotkey = update_hotkey
        self.save_settings = save_settings
        self.hide_tray_icon_var = hide_tray_icon_var
        self.hide_tray_icon_button = None
        self.replace_newline_var = replace_newline_var

        self.version = CURRENT_VERSION
        self.copyright_info = "Copyright 2026 FueTsui. All rights reserved."

        # Use ThemeManager singleton
        self._theme_mgr = ThemeManager.instance()

        self._apply_global_style()
        self.init_ui()

        self.hotkey_edit.installEventFilter(self)
        self.translate_hotkey_edit.installEventFilter(self)

        # 初始化时检测热键冲突状态（延迟 200ms 确保 hotkey_handler 已初始化）
        self._hotkey_check_retry_count = 0
        QTimer.singleShot(200, self._check_initial_hotkey_conflicts)

    def _check_initial_hotkey_conflicts(self):
        """初始化时检测当前热键的冲突状态"""
        # 检查 hotkey_handler 是否已初始化
        parent = self.parent()
        if not parent or not hasattr(parent, 'hotkey_handler') or parent.hotkey_handler is None:
            # 如果未初始化，重试最多 5 次（每次 200ms）
            self._hotkey_check_retry_count += 1
            if self._hotkey_check_retry_count < 5:
                logging.debug(f"hotkey_handler 未就绪，将重试 ({self._hotkey_check_retry_count}/5)")
                QTimer.singleShot(200, self._check_initial_hotkey_conflicts)
            else:
                logging.warning("hotkey_handler 初始化超时，跳过热键冲突检测")
            return

        # 检测识别热键
        self._check_and_update_hotkey_conflict(
            self.hotkey_var, 'ocr',
            self.hotkey_conflict_label,
            self.hotkey_edit
        )
        # 检测翻译热键
        self._check_and_update_hotkey_conflict(
            self.translate_hotkey_var, 'translate',
            self.translate_hotkey_conflict_label,
            self.translate_hotkey_edit
        )

    def _is_dark_theme(self):
        """检测当前是否为深色主题（使用 ThemeManager）"""
        return self._theme_mgr.is_dark_theme()

    def _start_theme_monitor(self):
        """启动主题监控 - 连接到 ThemeManager 信号"""
        # 使用 Qt.UniqueConnection 避免重复连接
        try:
            self._theme_mgr.theme_changed.connect(
                self._on_theme_changed_signal,
                Qt.UniqueConnection
            )
        except TypeError:
            # 已经连接过了
            pass

    def _stop_theme_monitor(self):
        """停止主题监控 - 断开信号连接"""
        try:
            self._theme_mgr.theme_changed.disconnect(self._on_theme_changed_signal)
        except (TypeError, RuntimeError):
            # 未连接或已断开
            pass

    def closeEvent(self, event):
        """关闭窗口时断开信号连接"""
        self._stop_theme_monitor()
        super().closeEvent(event)

    def _on_theme_changed_signal(self, is_dark: bool):
        """Handle theme change from ThemeManager."""
        self._on_theme_changed()

    def _apply_global_style(self, force=False):
        """应用全局样式"""
        is_dark = self._theme_mgr.is_dark_theme()

        # 如果主题没有改变，跳过重新应用
        if not force and hasattr(self, '_last_theme_dark') and self._last_theme_dark == is_dark:
            return

        self._last_theme_dark = is_dark
        colors = self._theme_mgr.get_colors()
        self._colors = colors

        self.setStyleSheet(f"""
            QDialog {{
                background-color: {colors['bg']};
            }}
            QWidget {{
                font-family: "Microsoft YaHei", "Segoe UI", sans-serif;
            }}
            QLabel {{
                color: {colors['text']};
                font-size: 14px;
                background: transparent;
            }}
            QLineEdit {{
                background-color: {colors['content_bg']};
                border: 1px solid {colors['border']};
                border-radius: 6px;
                padding: 6px 12px;
                font-size: 13px;
                color: {colors['text']};
            }}
            QLineEdit:focus {{
                border: 1px solid {colors['primary']};
            }}
        """)

    def changeEvent(self, event):
        """监听主题变化"""
        if event.type() == QEvent.PaletteChange:
            # 强制刷新 ThemeManager 缓存，确保获取最新主题状态
            # 因为 is_dark_theme() 返回的是缓存值，可能在 PaletteChange 事件到达时尚未更新
            self._theme_mgr.force_refresh()
            # 现在 is_dark_theme() 会返回最新值
            current_dark = self._theme_mgr.is_dark_theme()
            if not hasattr(self, '_last_theme_dark') or self._last_theme_dark != current_dark:
                self._on_theme_changed()
        super().changeEvent(event)

    def _on_theme_changed(self):
        """主题变化时更新样式"""
        current_dark = self._is_dark_theme()

        # 检查是否真正需要更新
        if hasattr(self, '_last_theme_dark') and self._last_theme_dark == current_dark:
            return

        logging.debug(f"SettingsWindow 主题更新: {'深色' if current_dark else '浅色'}")

        # 更新所有样式
        self._apply_global_style(force=True)
        self._apply_nav_style()
        self._update_content_pages_style()
        self._update_all_button_styles()

        # 强制刷新热键状态的显示
        self._check_and_update_hotkey_conflict(
            self.hotkey_edit.text(), 'ocr',
            self.hotkey_conflict_label, self.hotkey_edit
        )
        self._check_and_update_hotkey_conflict(
            self.translate_hotkey_edit.text(), 'translate',
            self.translate_hotkey_conflict_label, self.translate_hotkey_edit
        )

    def _update_all_button_styles(self):
        """更新所有状态按钮的样式"""
        self.update_autostart_button_style()
        self.update_hide_tray_icon_button_style()
        self.update_baidu_ocr_btn_style()
        self.update_tencent_ocr_btn_style()
        self.update_baidu_trans_btn_style()
        self.update_tencent_trans_btn_style()
        self.update_replace_newline_button_style()
        self._update_about_page_style()
        self._update_save_button_style()
        self._update_bottom_widget_style()

    def _update_hotkey_styles(self):
        """更新热键输入框和冲突标签的样式"""
        # 默认输入框样式（统一取自 ThemeManager）
        default_edit_style = self._theme_mgr.get_line_edit_style()

        # 更新识别热键
        if hasattr(self, 'hotkey_conflict_label') and hasattr(self, 'hotkey_edit'):
            if self.hotkey_conflict_label.isVisible():
                label_text = self.hotkey_conflict_label.text()
                if label_text == "已注册":
                    self._show_registered_label(self.hotkey_conflict_label, self.hotkey_edit)
                else:
                    self._show_conflict_label(self.hotkey_conflict_label, self.hotkey_edit, label_text)
            else:
                self.hotkey_edit.setStyleSheet(default_edit_style)

        # 更新翻译热键
        if hasattr(self, 'translate_hotkey_conflict_label') and hasattr(self, 'translate_hotkey_edit'):
            if self.translate_hotkey_conflict_label.isVisible():
                label_text = self.translate_hotkey_conflict_label.text()
                if label_text == "已注册":
                    self._show_registered_label(self.translate_hotkey_conflict_label, self.translate_hotkey_edit)
                else:
                    self._show_conflict_label(self.translate_hotkey_conflict_label, self.translate_hotkey_edit, label_text)
            else:
                self.translate_hotkey_edit.setStyleSheet(default_edit_style)

    def init_ui(self):
        main_layout = QVBoxLayout()
        self.setLayout(main_layout)
        main_layout.setContentsMargins(0, 0, 0, 0)
        main_layout.setSpacing(0)

        # 上部：左侧导航 + 右侧内容
        upper_widget = QWidget()
        upper_layout = QHBoxLayout()
        upper_layout.setContentsMargins(0, 0, 0, 0)
        upper_layout.setSpacing(0)
        upper_widget.setLayout(upper_layout)
        main_layout.addWidget(upper_widget, stretch=1)

        # ================= 左侧导航栏 (使用 QListWidget) =================
        self.nav_list = QListWidget()
        self.nav_list.setFixedWidth(100)
        self.nav_list.setFrameShape(QFrame.NoFrame)
        self._apply_nav_style()

        nav_items = ["常规", "接口", "格式", "快捷键", "关于"]
        for item_text in nav_items:
            item = QListWidgetItem(item_text)
            item.setSizeHint(QSize(100, 42))
            self.nav_list.addItem(item)

        self.nav_list.currentRowChanged.connect(self.stacked_widget_set_current_index)
        upper_layout.addWidget(self.nav_list)

        # ================= 右侧内容区 =================
        self.content_container = QWidget()
        content_layout = QVBoxLayout(self.content_container)
        content_layout.setContentsMargins(0, 0, 0, 0)
        content_layout.setSpacing(0)

        self.stacked_widget = QStackedWidget()
        content_layout.addWidget(self.stacked_widget)
        upper_layout.addWidget(self.content_container)

        # ================= 页面1: 常规 =================
        self.general_page = QWidget()
        general_layout = QGridLayout()
        general_layout.setContentsMargins(30, 30, 30, 30)
        general_layout.setVerticalSpacing(20)
        general_layout.setHorizontalSpacing(20)
        self.general_page.setLayout(general_layout)

        row = 0
        # 开机启动
        autostart_label = QLabel("开机启动")
        general_layout.addWidget(autostart_label, row, 0)
        self.autostart_enabled = is_autostart_enabled()
        self.autostart_button = QPushButton()
        self.autostart_button.setFixedSize(80, 32)
        self.autostart_button.setCursor(Qt.PointingHandCursor)
        self.autostart_button.clicked.connect(self.toggle_autostart)
        general_layout.addWidget(self.autostart_button, row, 1, Qt.AlignRight)
        self.update_autostart_button_style()
        row += 1

        # 托盘图标
        hide_tray_icon_label = QLabel("托盘图标")
        general_layout.addWidget(hide_tray_icon_label, row, 0)
        self.hide_tray_icon_button = QPushButton()
        self.hide_tray_icon_button.setFixedSize(80, 32)
        self.hide_tray_icon_button.setCursor(Qt.PointingHandCursor)
        self.hide_tray_icon_button.clicked.connect(self.toggle_hide_tray_icon)
        general_layout.addWidget(self.hide_tray_icon_button, row, 1, Qt.AlignRight)
        self.update_hide_tray_icon_button_style()
        general_layout.setRowStretch(row + 1, 1)

        self.stacked_widget.addWidget(self.general_page)

        # ================= 页面2: 接口 =================
        self.interface_page = QWidget()
        interface_layout = QGridLayout()
        interface_layout.setContentsMargins(30, 30, 30, 30)
        interface_layout.setVerticalSpacing(18)
        interface_layout.setHorizontalSpacing(20)
        self.interface_page.setLayout(interface_layout)

        row = 0
        # 百度识别
        baidu_ocr_label = QLabel("百度识别")
        interface_layout.addWidget(baidu_ocr_label, row, 0)
        self.baidu_ocr_btn = QPushButton("配置")
        self.baidu_ocr_btn.setFixedSize(80, 32)
        self.baidu_ocr_btn.setCursor(Qt.PointingHandCursor)
        self.baidu_ocr_btn.clicked.connect(self.open_baidu_ocr_config)
        interface_layout.addWidget(self.baidu_ocr_btn, row, 1, Qt.AlignRight)
        self.update_baidu_ocr_btn_style()
        row += 1

        # 腾讯识别
        tencent_ocr_label = QLabel("腾讯识别")
        interface_layout.addWidget(tencent_ocr_label, row, 0)
        self.tencent_ocr_btn = QPushButton("配置")
        self.tencent_ocr_btn.setFixedSize(80, 32)
        self.tencent_ocr_btn.setCursor(Qt.PointingHandCursor)
        self.tencent_ocr_btn.clicked.connect(self.open_tencent_ocr_config)
        interface_layout.addWidget(self.tencent_ocr_btn, row, 1, Qt.AlignRight)
        self.update_tencent_ocr_btn_style()
        row += 1

        # 百度翻译
        baidu_trans_label = QLabel("百度翻译")
        interface_layout.addWidget(baidu_trans_label, row, 0)
        self.baidu_trans_btn = QPushButton("配置")
        self.baidu_trans_btn.setFixedSize(80, 32)
        self.baidu_trans_btn.setCursor(Qt.PointingHandCursor)
        self.baidu_trans_btn.clicked.connect(self.open_baidu_trans_config)
        interface_layout.addWidget(self.baidu_trans_btn, row, 1, Qt.AlignRight)
        self.update_baidu_trans_btn_style()
        row += 1

        # 腾讯翻译
        tencent_trans_label = QLabel("腾讯翻译")
        interface_layout.addWidget(tencent_trans_label, row, 0)
        self.tencent_trans_btn = QPushButton("配置")
        self.tencent_trans_btn.setFixedSize(80, 32)
        self.tencent_trans_btn.setCursor(Qt.PointingHandCursor)
        self.tencent_trans_btn.clicked.connect(self.open_tencent_trans_config)
        interface_layout.addWidget(self.tencent_trans_btn, row, 1, Qt.AlignRight)
        self.update_tencent_trans_btn_style()
        interface_layout.setRowStretch(row + 1, 1)

        self.stacked_widget.addWidget(self.interface_page)

        # ================= 页面3: 格式 =================
        self.format_page = QWidget()
        format_layout = QGridLayout()
        format_layout.setContentsMargins(30, 30, 30, 30)
        format_layout.setVerticalSpacing(18)
        format_layout.setHorizontalSpacing(20)
        self.format_page.setLayout(format_layout)

        row = 0
        # 文本处理
        replace_newline_label = QLabel("文本处理")
        replace_newline_label.setToolTip("开启后，识别文本中的换行符将自动替换为空格")
        format_layout.addWidget(replace_newline_label, row, 0)
        self.replace_newline_button = QPushButton()
        self.replace_newline_button.setFixedSize(80, 32)
        self.replace_newline_button.setCursor(Qt.PointingHandCursor)
        self.replace_newline_button.setToolTip("开启后，识别文本中的换行符将自动替换为空格")
        self.replace_newline_button.clicked.connect(self.toggle_replace_newline)
        format_layout.addWidget(self.replace_newline_button, row, 1, Qt.AlignRight)
        self.update_replace_newline_button_style()
        format_layout.setRowStretch(row + 1, 1)

        self.stacked_widget.addWidget(self.format_page)

        # ================= 页面4: 快捷键 =================
        self.hotkey_page = QWidget()
        hotkey_layout = QGridLayout()
        hotkey_layout.setContentsMargins(30, 30, 30, 30)
        hotkey_layout.setVerticalSpacing(18)
        hotkey_layout.setHorizontalSpacing(20)
        self.hotkey_page.setLayout(hotkey_layout)

        row = 0
        # 识别热键
        hotkey_label = QLabel("识别热键")
        hotkey_layout.addWidget(hotkey_label, row, 0)

        # 识别热键输入框 + 冲突标签容器
        ocr_hotkey_container = QWidget()
        ocr_hotkey_layout = QHBoxLayout(ocr_hotkey_container)
        ocr_hotkey_layout.setContentsMargins(0, 0, 0, 0)
        ocr_hotkey_layout.setSpacing(8)

        self.hotkey_edit = QLineEdit(self.hotkey_var)
        self.hotkey_edit.setFixedHeight(34)
        self.hotkey_edit.setFixedWidth(120)
        self.hotkey_edit.setAlignment(Qt.AlignCenter)
        ocr_hotkey_layout.addWidget(self.hotkey_edit)

        self.hotkey_conflict_label = QLabel()
        self.hotkey_conflict_label.setFixedHeight(34)
        self.hotkey_conflict_label.setAlignment(Qt.AlignCenter)
        self.hotkey_conflict_label.hide()
        ocr_hotkey_layout.addWidget(self.hotkey_conflict_label)

        ocr_hotkey_layout.addStretch()
        hotkey_layout.addWidget(ocr_hotkey_container, row, 1, Qt.AlignRight)
        row += 1

        # 翻译热键
        translate_hotkey_label = QLabel("翻译热键")
        hotkey_layout.addWidget(translate_hotkey_label, row, 0)

        # 翻译热键输入框 + 冲突标签容器
        trans_hotkey_container = QWidget()
        trans_hotkey_layout = QHBoxLayout(trans_hotkey_container)
        trans_hotkey_layout.setContentsMargins(0, 0, 0, 0)
        trans_hotkey_layout.setSpacing(8)

        self.translate_hotkey_edit = QLineEdit(self.translate_hotkey_var)
        self.translate_hotkey_edit.setFixedHeight(34)
        self.translate_hotkey_edit.setFixedWidth(120)
        self.translate_hotkey_edit.setAlignment(Qt.AlignCenter)
        trans_hotkey_layout.addWidget(self.translate_hotkey_edit)

        self.translate_hotkey_conflict_label = QLabel()
        self.translate_hotkey_conflict_label.setFixedHeight(34)
        self.translate_hotkey_conflict_label.setAlignment(Qt.AlignCenter)
        self.translate_hotkey_conflict_label.hide()
        trans_hotkey_layout.addWidget(self.translate_hotkey_conflict_label)

        trans_hotkey_layout.addStretch()
        hotkey_layout.addWidget(trans_hotkey_container, row, 1, Qt.AlignRight)
        hotkey_layout.setRowStretch(row + 1, 1)

        self.stacked_widget.addWidget(self.hotkey_page)

        # ================= 页面5: 关于 =================
        self.about_page = QWidget()
        about_layout = QVBoxLayout()
        about_layout.setContentsMargins(30, 50, 30, 25)
        about_layout.setSpacing(0)
        self.about_page.setLayout(about_layout)

        # 程序名称
        self.name_label = QLabel("SightOCR")
        self.name_label.setAlignment(Qt.AlignCenter)
        about_layout.addWidget(self.name_label)

        about_layout.addSpacing(8)

        # 版本号
        self.about_version_label = QLabel(self.version)
        self.about_version_label.setAlignment(Qt.AlignCenter)
        about_layout.addWidget(self.about_version_label)

        about_layout.addSpacing(25)

        # 链接按钮区域 - 上下排列
        github_url = "https://github.com/FueTsui/SightOCR"
        self.github_btn = QPushButton("官网首页")
        self.github_btn.setFixedSize(120, 36)
        self.github_btn.setCursor(Qt.PointingHandCursor)
        self.github_btn.clicked.connect(lambda: webbrowser.open(github_url))

        qq_group_url = "http://qm.qq.com/cgi-bin/qm/qr?_wv=1027&k=5PkXysHbDS-RVvXJO_AE7OkFEZrIaFYN&authKey=OZ7pUAm4Ek2ZUKmurSP5v2w9lcbQN2%2BnoiGdIJzM0ZT5QsFkrV0CICvkw9C7qIWS&noverify=0&group_code=175332502"
        self.qq_btn = QPushButton("摸鱼搭子")
        self.qq_btn.setFixedSize(120, 36)
        self.qq_btn.setCursor(Qt.PointingHandCursor)
        self.qq_btn.clicked.connect(lambda: webbrowser.open(qq_group_url))

        # 居中容器
        github_layout = QHBoxLayout()
        github_layout.addStretch()
        github_layout.addWidget(self.github_btn)
        github_layout.addStretch()
        about_layout.addLayout(github_layout)

        about_layout.addSpacing(12)

        qq_layout = QHBoxLayout()
        qq_layout.addStretch()
        qq_layout.addWidget(self.qq_btn)
        qq_layout.addStretch()
        about_layout.addLayout(qq_layout)

        about_layout.addStretch(1)

        # 版权信息
        self.copyright_label = QLabel(self.copyright_info)
        self.copyright_label.setAlignment(Qt.AlignCenter)
        about_layout.addWidget(self.copyright_label)

        self.stacked_widget.addWidget(self.about_page)

        # ================= 底部保存按钮 =================
        self.bottom_widget = QWidget()
        bottom_layout = QHBoxLayout(self.bottom_widget)
        bottom_layout.setContentsMargins(15, 10, 15, 10)
        bottom_layout.setSpacing(10)
        main_layout.addWidget(self.bottom_widget)

        self.version_label = QLabel(f"版本: {self.version}")
        bottom_layout.addWidget(self.version_label)
        bottom_layout.addStretch()

        self.save_btn = QPushButton("保存")
        self.save_btn.setFixedSize(80, 34)
        self.save_btn.setCursor(Qt.PointingHandCursor)
        self.save_btn.clicked.connect(self.save_all_settings)
        bottom_layout.addWidget(self.save_btn)

        # 应用动态样式
        self._update_content_pages_style()
        self._update_about_page_style()
        self._update_save_button_style()
        self._update_bottom_widget_style()

        # 默认显示第一个页面
        self.nav_list.setCurrentRow(0)

        # 启动主题监控
        self._start_theme_monitor()

    def stacked_widget_set_current_index(self, index):
        """切换页面"""
        self.stacked_widget.setCurrentIndex(index)

    def _apply_nav_style(self):
        """应用导航栏样式"""
        colors = getattr(self, '_colors', self._theme_mgr.get_colors())
        self.nav_list.setStyleSheet(f"""
            QListWidget {{
                background-color: {colors['bg']};
                border: none;
                outline: none;
                padding: 10px 0px;
            }}
            QListWidget::item {{
                height: 40px;
                padding-left: 15px;
                border: none;
                color: {colors['text']};
                font-size: 13px;
            }}
            QListWidget::item:selected {{
                background-color: {colors['nav_selected_bg']};
                border-left: 3px solid {colors['primary']};
                color: {colors['primary']};
                font-weight: 500;
            }}
            QListWidget::item:hover:!selected {{
                background-color: {colors['hover_bg']};
            }}
        """)

    def _update_about_page_style(self):
        """更新关于页面样式"""
        colors = getattr(self, '_colors', self._theme_mgr.get_colors())
        btn_style = f"""
            QPushButton {{
                background-color: {colors['content_bg']};
                border: 1px solid {colors['border']};
                border-radius: 6px;
                color: {colors['text']};
                font-size: 13px;
            }}
            QPushButton:hover {{
                background-color: {colors['hover_bg']};
            }}
        """
        if hasattr(self, 'github_btn'):
            self.github_btn.setStyleSheet(btn_style)
        if hasattr(self, 'qq_btn'):
            self.qq_btn.setStyleSheet(btn_style)
        if hasattr(self, 'about_version_label'):
            self.about_version_label.setStyleSheet(f"font-size: 13px; color: {colors['text_light']};")

    def _update_save_button_style(self):
        """更新保存按钮样式"""
        colors = getattr(self, '_colors', self._theme_mgr.get_colors())
        if hasattr(self, 'save_btn'):
            self.save_btn.setStyleSheet(f"""
                QPushButton {{
                    background-color: {colors['content_bg']};
                    border: 1px solid {colors['border']};
                    border-radius: 6px;
                    color: {colors['text']};
                    font-size: 13px;
                }}
                QPushButton:hover {{
                    background-color: {colors['hover_bg']};
                }}
            """)

    def _update_bottom_widget_style(self):
        """更新底部区域样式"""
        colors = getattr(self, '_colors', self._theme_mgr.get_colors())
        if hasattr(self, 'bottom_widget'):
            self.bottom_widget.setStyleSheet(f"background-color: {colors['bg']};")
        if hasattr(self, 'version_label'):
            self.version_label.setStyleSheet(f"color: {colors['text_light']}; font-size: 11px;")
        if hasattr(self, 'copyright_label'):
            self.copyright_label.setStyleSheet(f"color: {colors['text_light']}; font-size: 11px;")

    def _update_content_pages_style(self):
        """更新右侧内容页面的样式"""
        colors = getattr(self, '_colors', self._theme_mgr.get_colors())
        content_bg = colors['content_bg']

        # 更新内容容器和堆栈
        if hasattr(self, 'content_container'):
            self.content_container.setStyleSheet(f"background-color: {content_bg};")
        if hasattr(self, 'stacked_widget'):
            self.stacked_widget.setStyleSheet(f"background-color: {content_bg};")

        # 更新各个页面背景
        pages = [self.general_page, self.interface_page, self.format_page, self.hotkey_page, self.about_page]
        for page in pages:
            if page:
                page.setStyleSheet(f"background-color: {content_bg};")

        # 更新关于页面的标题样式
        if hasattr(self, 'name_label'):
            self.name_label.setStyleSheet(f"font-size: 24px; font-weight: bold; color: {colors['text']};")

    def _get_status_button_style(self, is_active, active_text="已开启", inactive_text="已关闭"):
        """获取状态按钮样式（统一委托给 ThemeManager，避免重复定义）。

        active_text/inactive_text 仅为历史调用兼容保留，按钮文案由调用方
        通过 setText 设置，样式本身与文案无关。
        """
        return self._theme_mgr.get_status_button_style(is_active)

    def open_baidu_ocr_config(self):
        """打开百度识别 API 配置对话框"""
        fields = [
            {
                'key': 'api_key',
                'label': 'API Key',
                'value': self.api_key_var,
                'placeholder': '请输入你的 API Key'
            },
            {
                'key': 'secret_key',
                'label': 'Secret Key',
                'value': self.secret_key_var,
                'placeholder': '请输入你的 Secret Key'
            }
        ]
        links = [
            {'label': '获取密钥', 'url': 'https://console.bce.baidu.com/ai/#/ai/ocr/app/list'}
        ]

        dialog = ApiConfigDialog(self, "百度识别 API 配置", fields, links)
        dialog.config_saved.connect(self.on_baidu_ocr_config_saved)
        dialog.exec()

    def on_baidu_ocr_config_saved(self, config):
        """百度识别配置保存回调"""
        self.api_key_var = config.get('api_key', '')
        self.secret_key_var = config.get('secret_key', '')
        self.update_baidu_ocr_btn_style()
        self._save_to_config_file()
        logging.debug(f"百度识别配置已更新: api_key={bool(self.api_key_var)}, secret_key={bool(self.secret_key_var)}")

    def update_baidu_ocr_btn_style(self):
        """更新百度识别按钮样式，显示配置状态"""
        is_configured = bool(self.api_key_var and self.secret_key_var)
        self.baidu_ocr_btn.setText("已配置" if is_configured else "配置")
        self.baidu_ocr_btn.setStyleSheet(self._get_status_button_style(is_configured))

    def open_tencent_ocr_config(self):
        """打开腾讯识别 API 配置对话框"""
        fields = [
            {
                'key': 'secret_id',
                'label': 'SecretId',
                'value': self.tencent_secret_id_var,
                'placeholder': '请输入你的 SecretId'
            },
            {
                'key': 'secret_key',
                'label': 'SecretKey',
                'value': self.tencent_secret_key_var,
                'placeholder': '请输入你的 SecretKey'
            }
        ]
        links = [
            {'label': '获取密钥', 'url': 'https://console.cloud.tencent.com/cam/capi'}
        ]

        dialog = ApiConfigDialog(self, "腾讯识别 API 配置", fields, links)
        dialog.config_saved.connect(self.on_tencent_ocr_config_saved)
        dialog.exec()

    def on_tencent_ocr_config_saved(self, config):
        """腾讯识别配置保存回调"""
        self.tencent_secret_id_var = config.get('secret_id', '')
        self.tencent_secret_key_var = config.get('secret_key', '')
        self.update_tencent_ocr_btn_style()
        self._save_to_config_file()
        logging.debug(f"腾讯识别配置已更新: secret_id={bool(self.tencent_secret_id_var)}")

    def update_tencent_ocr_btn_style(self):
        """更新腾讯识别按钮样式，显示配置状态"""
        is_configured = bool(self.tencent_secret_id_var and self.tencent_secret_key_var)
        self.tencent_ocr_btn.setText("已配置" if is_configured else "配置")
        self.tencent_ocr_btn.setStyleSheet(self._get_status_button_style(is_configured))

    def open_baidu_trans_config(self):
        """打开百度翻译 API 配置对话框"""
        fields = [
            {
                'key': 'appid',
                'label': 'APP ID',
                'value': self.baidu_trans_appid_var,
                'placeholder': '请输入你的 APP ID'
            },
            {
                'key': 'appkey',
                'label': 'APP Key',
                'value': self.baidu_trans_appkey_var,
                'placeholder': '请输入你的 APP Key'
            }
        ]
        links = [
            {'label': '获取密钥', 'url': 'https://fanyi-api.baidu.com/manage/developer'}
        ]

        dialog = ApiConfigDialog(self, "百度翻译 API 配置", fields, links)
        dialog.config_saved.connect(self.on_baidu_trans_config_saved)
        dialog.exec()

    def on_baidu_trans_config_saved(self, config):
        """百度翻译配置保存回调"""
        self.baidu_trans_appid_var = config.get('appid', '')
        self.baidu_trans_appkey_var = config.get('appkey', '')
        self.update_baidu_trans_btn_style()
        self._save_to_config_file()
        logging.debug(f"百度翻译配置已更新: appid={bool(self.baidu_trans_appid_var)}")

    def update_baidu_trans_btn_style(self):
        """更新百度翻译按钮样式，显示配置状态"""
        is_configured = bool(self.baidu_trans_appid_var and self.baidu_trans_appkey_var)
        self.baidu_trans_btn.setText("已配置" if is_configured else "配置")
        self.baidu_trans_btn.setStyleSheet(self._get_status_button_style(is_configured))

    def open_tencent_trans_config(self):
        """打开腾讯翻译 API 配置对话框"""
        fields = [
            {
                'key': 'secret_id',
                'label': 'SecretId',
                'value': self.tencent_trans_secret_id_var,
                'placeholder': '请输入你的 SecretId'
            },
            {
                'key': 'secret_key',
                'label': 'SecretKey',
                'value': self.tencent_trans_secret_key_var,
                'placeholder': '请输入你的 SecretKey'
            }
        ]
        links = [
            {'label': '获取密钥', 'url': 'https://console.cloud.tencent.com/cam/capi'}
        ]

        dialog = ApiConfigDialog(self, "腾讯翻译 API 配置", fields, links)
        dialog.config_saved.connect(self.on_tencent_trans_config_saved)
        dialog.exec()

    def on_tencent_trans_config_saved(self, config):
        """腾讯翻译配置保存回调"""
        self.tencent_trans_secret_id_var = config.get('secret_id', '')
        self.tencent_trans_secret_key_var = config.get('secret_key', '')
        self.update_tencent_trans_btn_style()
        self._save_to_config_file()
        logging.debug(f"腾讯翻译配置已更新: secret_id={bool(self.tencent_trans_secret_id_var)}")

    def update_tencent_trans_btn_style(self):
        """更新腾讯翻译按钮样式，显示配置状态"""
        is_configured = bool(self.tencent_trans_secret_id_var and self.tencent_trans_secret_key_var)
        self.tencent_trans_btn.setText("已配置" if is_configured else "配置")
        self.tencent_trans_btn.setStyleSheet(self._get_status_button_style(is_configured))

    def showEvent(self, event):
        """显示时居中到父窗口"""
        super().showEvent(event)
        self.move_to_center()

    def move_to_center(self):
        """将窗口移动到父窗口的中央"""
        if self.parent():
            parent_rect = self.parent().geometry()
            self.move(
                parent_rect.x() + (parent_rect.width() - self.width()) // 2,
                parent_rect.y() + (parent_rect.height() - self.height()) // 2
            )

    def get_key_sequence(self, event):
        modifiers = []
        if event.modifiers() & Qt.ControlModifier:
            modifiers.append('Ctrl')
        if event.modifiers() & Qt.AltModifier:
            modifiers.append('Alt')
        if event.modifiers() & Qt.ShiftModifier:
            modifiers.append('Shift')
        if event.modifiers() & Qt.MetaModifier:
            modifiers.append('Meta')

        key = event.key()
        if key in (Qt.Key_Control, Qt.Key_Shift, Qt.Key_Alt, Qt.Key_Meta):
            return None

        key_name = QKeySequence(key).toString()
        if not key_name:
            return None

        if key_name in modifiers:
            return None

        key_sequence = '+'.join(modifiers + [key_name])
        return key_sequence

    def eventFilter(self, obj, event):
        if event.type() == QEvent.KeyPress:
            key_sequence = self.get_key_sequence(event)
            if key_sequence:
                if obj == self.hotkey_edit:
                    self.hotkey_var = key_sequence
                    self.hotkey_edit.setText(self.hotkey_var)
                    # 检测冲突
                    self._check_and_update_hotkey_conflict(
                        key_sequence, 'ocr',
                        self.hotkey_conflict_label,
                        self.hotkey_edit
                    )
                    self.update_hotkey(self.hotkey_var, self.translate_hotkey_var, False)
                elif obj == self.translate_hotkey_edit:
                    self.translate_hotkey_var = key_sequence
                    self.translate_hotkey_edit.setText(self.translate_hotkey_var)
                    # 检测冲突
                    self._check_and_update_hotkey_conflict(
                        key_sequence, 'translate',
                        self.translate_hotkey_conflict_label,
                        self.translate_hotkey_edit
                    )
                    self.update_hotkey(self.hotkey_var, self.translate_hotkey_var, False)
                return True
            else:
                return False
        return super().eventFilter(obj, event)

    def _check_and_update_hotkey_conflict(self, hotkey_str, hotkey_type, conflict_label, edit_widget):
        """
        检测热键冲突并更新标签显示。

        Args:
            hotkey_str: 热键字符串
            hotkey_type: 热键类型 ('ocr' 或 'translate')
            conflict_label: 冲突标签控件
            edit_widget: 输入框控件
        """
        # 检测是否与另一个热键相同
        if hotkey_type == 'ocr' and hotkey_str == self.translate_hotkey_var:
            self._show_conflict_label(conflict_label, edit_widget, "已占用")
            return
        elif hotkey_type == 'translate' and hotkey_str == self.hotkey_var:
            self._show_conflict_label(conflict_label, edit_widget, "已占用")
            return

        # 尝试获取主窗口的热键处理器进行冲突检测
        try:
            parent = self.parent()
            if parent and hasattr(parent, 'hotkey_handler'):
                handler = parent.hotkey_handler

                # 检查是否是当前已注册的热键（被自己占用则显示已注册）
                current_ocr_hotkey = handler.get_registered_hotkey('OCR')
                current_trans_hotkey = handler.get_registered_hotkey('翻译')

                if hotkey_type == 'ocr' and hotkey_str == current_ocr_hotkey:
                    self._show_registered_label(conflict_label, edit_widget)
                    return
                elif hotkey_type == 'translate' and hotkey_str == current_trans_hotkey:
                    self._show_registered_label(conflict_label, edit_widget)
                    return

                # 检测冲突
                result = handler.check_hotkey_conflict(hotkey_str)
                if not result['available']:
                    self._show_conflict_label(conflict_label, edit_widget, "已占用")
                    return
        except Exception as e:
            logging.debug(f"热键冲突检测失败: {e}")

        # 无冲突，显示已注册
        self._show_registered_label(conflict_label, edit_widget)

    def _show_conflict_label(self, label, edit_widget, message):
        """显示冲突标签（红色），样式统一取自 ThemeManager"""
        label.setText(message)
        label.setStyleSheet(self._theme_mgr.get_status_label_style(error=True))
        label.show()
        edit_widget.setStyleSheet(self._theme_mgr.get_line_edit_style(error=True))

    def _show_registered_label(self, label, edit_widget):
        """显示已注册标签（绿色），样式统一取自 ThemeManager"""
        label.setText("已注册")
        label.setStyleSheet(self._theme_mgr.get_status_label_style(error=False))
        label.show()
        edit_widget.setStyleSheet(self._theme_mgr.get_line_edit_style(error=False))

    def toggle_autostart(self):
        self.autostart_enabled = not self.autostart_enabled
        set_autostart(self.autostart_enabled)
        self.update_autostart_button_style()

    def update_autostart_button_style(self):
        self.autostart_button.setText("已开启" if self.autostart_enabled else "已关闭")
        self.autostart_button.setStyleSheet(self._get_status_button_style(self.autostart_enabled))

    def toggle_hide_tray_icon(self):
        self.hide_tray_icon_var = not self.hide_tray_icon_var
        logging.debug(f"toggle_hide_tray_icon called. hide_tray_icon_var: {self.hide_tray_icon_var}")
        settings_to_save = self._build_settings_dict()
        self.parent().save_settings(settings_to_save)
        self.parent().update_tray_icon_visibility(self.hide_tray_icon_var)
        self.update_hide_tray_icon_button_style()

    def update_hide_tray_icon_button_style(self):
        # 托盘图标"已显示"是积极状态
        is_shown = not self.hide_tray_icon_var
        self.hide_tray_icon_button.setText("已显示" if is_shown else "已隐藏")
        self.hide_tray_icon_button.setStyleSheet(self._get_status_button_style(is_shown))

    def toggle_replace_newline(self):
        self.replace_newline_var = not self.replace_newline_var
        self._save_to_config_file()
        self.update_replace_newline_button_style()

    def update_replace_newline_button_style(self):
        self.replace_newline_button.setText("已启用" if self.replace_newline_var else "未启用")
        self.replace_newline_button.setStyleSheet(self._get_status_button_style(self.replace_newline_var))

    def _build_settings_dict(self):
        """构建设置字典"""
        return {
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

    def _save_to_config_file(self):
        """立即将当前配置保存到文件"""
        try:
            settings_to_save = self._build_settings_dict()
            self.parent().save_settings(settings_to_save)
            logging.debug("API配置已保存到文件")
        except Exception as e:
            logging.error(f"保存API配置到文件失败: {e}")

    def save_all_settings(self):
        try:
            self.hotkey_var = self.hotkey_edit.text()
            self.translate_hotkey_var = self.translate_hotkey_edit.text()

            settings_to_save = self._build_settings_dict()
            self.parent().save_settings(settings_to_save)

            self.close()
        except Exception as e:
            logging.error(f"保存设置时发生错误: {e}", exc_info=True)
            QMessageBox.warning(self, "保存失败", f"保存设置时发生错误:\n{e}")




class MaskedLineEdit(QLineEdit):
    def __init__(self, text):
        super().__init__()
        self.real_text = text
        self.setEchoMode(QLineEdit.Password)
        self.showing_real_text = False
        self._theme_mgr = ThemeManager.instance()
        self.update_display()

    def focusInEvent(self, event):
        super().focusInEvent(event)
        self.setEchoMode(QLineEdit.Password)

    def focusOutEvent(self, event):
        super().focusOutEvent(event)
        self.setEchoMode(QLineEdit.Password)
        if not hasattr(self, 'showing_real_text'):
            self.showing_real_text = False
        else:
            self.showing_real_text = False
        self.update_display()

    def mousePressEvent(self, event):
        super().mousePressEvent(event)
        if not hasattr(self, 'showing_real_text'):
            self.showing_real_text = False

        if self.echoMode() == QLineEdit.Password:
            self.setEchoMode(QLineEdit.Normal)
            self.setText(self.real_text)
            self.showing_real_text = True
            self._apply_theme_style()
        else:
            self.setEchoMode(QLineEdit.Password)
            self.update_display()
            self.showing_real_text = False

    def keyPressEvent(self, event):
        super().keyPressEvent(event)
        self.real_text = self.text()

    def _apply_theme_style(self):
        """应用主题颜色到输入框"""
        colors = self._theme_mgr.get_colors()
        if self.real_text and not self.showing_real_text:
            # 掩码显示状态：使用浅色文字
            self.setStyleSheet(f"color: {colors['text_light']};")
        else:
            # 正常显示状态：使用标准文字颜色
            self.setStyleSheet(f"color: {colors['text']};")

    def update_display(self):
        if not hasattr(self, 'showing_real_text'):
            self.showing_real_text = False

        if self.real_text and not self.showing_real_text:
            self.setText('*' * 30)
            self._apply_theme_style()
        elif not self.real_text:
            self.clear()
            self._apply_theme_style()
        else:
            self._apply_theme_style()