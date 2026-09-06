# theme_manager.py
"""
Singleton ThemeManager for unified theme detection and monitoring.
Eliminates duplicate theme detection code across windows.
"""

import logging
from PySide6.QtCore import QObject, Signal, QTimer


class ThemeManager(QObject):
    """
    Singleton manager for Windows theme detection and monitoring.

    Usage:
        theme_mgr = ThemeManager.instance()
        is_dark = theme_mgr.is_dark_theme()
        colors = theme_mgr.get_colors()
        theme_mgr.theme_changed.connect(on_theme_changed)
        theme_mgr.start_monitoring()
    """

    # Signal emitted when theme changes (is_dark: bool)
    theme_changed = Signal(bool)

    _instance = None
    _initialized = False

    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def __init__(self):
        if ThemeManager._initialized:
            return
        super().__init__()
        ThemeManager._initialized = True

        self._current_is_dark = None
        self._timer = None
        self._monitoring = False

        # Initialize current theme state
        self._current_is_dark = self._detect_theme()
        logging.debug(f"ThemeManager initialized. Dark theme: {self._current_is_dark}")

    @classmethod
    def instance(cls) -> 'ThemeManager':
        """Get the singleton instance."""
        if cls._instance is None:
            cls._instance = cls()
        return cls._instance

    def _detect_theme(self) -> bool:
        """Detect if Windows is using dark theme via registry."""
        try:
            import winreg
            registry = winreg.ConnectRegistry(None, winreg.HKEY_CURRENT_USER)
            key = winreg.OpenKey(
                registry,
                r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"
            )
            value, _ = winreg.QueryValueEx(key, "AppsUseLightTheme")
            winreg.CloseKey(key)
            return value == 0  # 0 = dark, 1 = light
        except Exception as e:
            logging.warning(f"Failed to detect theme from registry: {e}")
            # Fallback: assume light theme
            return False

    def is_dark_theme(self) -> bool:
        """Return whether the current theme is dark."""
        if self._current_is_dark is None:
            self._current_is_dark = self._detect_theme()
        return self._current_is_dark

    def get_colors(self) -> dict:
        """
        Get color scheme based on current theme.

        Returns dict with keys:
            bg, content_bg, text, text_light, border, primary, primary_hover,
            status_green, status_green_bg, hover_bg, nav_selected_bg,
            focus_border, menu_selected_bg, menu_selected_color, pressed_bg
        """
        is_dark = self.is_dark_theme()

        if is_dark:
            return {
                'bg': "#1E1E1E",
                'content_bg': "#2D2D2D",
                'text': "#E0E0E0",
                'text_light': "#A0A0A0",
                'border': "#404040",
                'primary': "#5B9BD5",
                'primary_hover': "#6BABEA",
                'status_green': "#4CAF50",
                'status_green_bg': "#1E3A1E",
                'status_red': "#EF5350",
                'status_red_bg': "#3D2020",
                'status_red_border': "#D32F2F",
                'hover_bg': "#3D3D3D",
                'nav_selected_bg': "#2D2D2D",
                'focus_border': "#5B9BD5",
                'menu_selected_bg': "#37373D",
                'menu_selected_color': "#5B9BD5",
                'pressed_bg': "#4D4D4D",
                'toggle_active_bg': "#2D4A6E",
                'toggle_active_hover_bg': "#3D5A7E",
                'scrollbar_thumb': "rgba(255, 255, 255, 18%)",
                'scrollbar_thumb_hover': "rgba(255, 255, 255, 32%)",
                'scrollbar_thumb_pressed': "rgba(255, 255, 255, 45%)",
            }
        else:
            return {
                'bg': "#F8F9FA",
                'content_bg': "#FFFFFF",
                'text': "#333333",
                'text_light': "#666666",
                'border': "#E0E0E0",
                'primary': "#4A90D9",
                'primary_hover': "#3A7FC8",
                'status_green': "#34A853",
                'status_green_bg': "#E6F4EA",
                'status_red': "#C62828",
                'status_red_bg': "#FFEBEE",
                'status_red_border': "#D32F2F",
                'hover_bg': "#F0F0F0",
                'nav_selected_bg': "#FFFFFF",
                'focus_border': "#4A90D9",
                'menu_selected_bg': "#E8F0FE",
                'menu_selected_color': "#1967D2",
                'pressed_bg': "#E8E8E8",
                'toggle_active_bg': "#E8F0FE",
                'toggle_active_hover_bg': "#D2E3FC",
                'scrollbar_thumb': "rgba(0, 0, 0, 18%)",
                'scrollbar_thumb_hover': "rgba(0, 0, 0, 32%)",
                'scrollbar_thumb_pressed': "rgba(0, 0, 0, 45%)",
            }

    def start_monitoring(self, interval_ms: int = 1000):
        """
        Start monitoring for theme changes.

        Args:
            interval_ms: Check interval in milliseconds (default: 1000)
        """
        if self._monitoring:
            return

        self._timer = QTimer(self)
        self._timer.timeout.connect(self._check_theme_change)
        self._timer.start(interval_ms)
        self._monitoring = True
        logging.debug(f"Theme monitoring started (interval: {interval_ms}ms)")

    def stop_monitoring(self):
        """Stop monitoring for theme changes."""
        if self._timer:
            self._timer.stop()
            self._timer = None
        self._monitoring = False
        logging.debug("Theme monitoring stopped")

    def _check_theme_change(self):
        """Check if theme has changed and emit signal if so."""
        new_is_dark = self._detect_theme()
        if new_is_dark != self._current_is_dark:
            self._current_is_dark = new_is_dark
            logging.info(f"Theme changed to {'dark' if new_is_dark else 'light'}")
            self.theme_changed.emit(new_is_dark)

    def force_refresh(self):
        """Force a theme refresh and emit signal only if theme changed."""
        new_is_dark = self._detect_theme()
        if new_is_dark != self._current_is_dark:
            self._current_is_dark = new_is_dark
            logging.info(f"Theme force refreshed to {'dark' if new_is_dark else 'light'}")
            self.theme_changed.emit(new_is_dark)

    def get_main_window_stylesheet(self) -> str:
        """Get the complete stylesheet for MainWindow."""
        colors = self.get_colors()
        return f"""
            QMainWindow {{
                background-color: {colors['bg']};
            }}
            QWidget {{
                background-color: {colors['bg']};
                color: {colors['text']};
                font-family: "Microsoft YaHei", "Segoe UI", sans-serif;
            }}
            QPushButton {{
                background-color: transparent;
                border: none;
                border-radius: 6px;
                padding: 5px 15px;
                color: {colors['text']};
                font-size: 13px;
            }}
            QPushButton:hover {{
                background-color: {colors['hover_bg']};
            }}
            QPushButton:pressed {{
                background-color: {colors['pressed_bg']};
            }}
            QToolButton {{
                background-color: transparent;
                border: none;
                border-radius: 6px;
                padding: 5px 15px;
                color: {colors['text']};
                font-size: 13px;
            }}
            QToolButton:hover {{
                background-color: {colors['hover_bg']};
            }}
            QToolButton:pressed {{
                background-color: {colors['pressed_bg']};
            }}
            QToolButton::menu-indicator {{
                image: none;
                width: 0px;
            }}
            QTextEdit {{
                background-color: {colors['content_bg']};
                border: 1px solid {colors['border']};
                border-radius: 8px;
                padding: 8px;
                font-size: 13px;
                color: {colors['text']};
            }}
            QTextEdit:focus {{
                border: 1px solid {colors['focus_border']};
            }}
            QTextEdit QScrollBar:vertical {{
                background: transparent;
                width: 10px;
                margin: 4px 2px 4px 2px;
            }}
            QTextEdit QScrollBar::handle:vertical {{
                background: {colors['scrollbar_thumb']};
                border-radius: 3px;
                min-height: 30px;
            }}
            QTextEdit QScrollBar::handle:vertical:hover {{
                background: {colors['scrollbar_thumb_hover']};
            }}
            QTextEdit QScrollBar::handle:vertical:pressed {{
                background: {colors['scrollbar_thumb_pressed']};
            }}
            QTextEdit QScrollBar::add-line:vertical,
            QTextEdit QScrollBar::sub-line:vertical {{
                height: 0px;
                background: none;
                border: none;
            }}
            QTextEdit QScrollBar::add-page:vertical,
            QTextEdit QScrollBar::sub-page:vertical {{
                background: transparent;
            }}
            QTextEdit QScrollBar:horizontal {{
                background: transparent;
                height: 10px;
                margin: 2px 4px 2px 4px;
            }}
            QTextEdit QScrollBar::handle:horizontal {{
                background: {colors['scrollbar_thumb']};
                border-radius: 3px;
                min-width: 30px;
            }}
            QTextEdit QScrollBar::handle:horizontal:hover {{
                background: {colors['scrollbar_thumb_hover']};
            }}
            QTextEdit QScrollBar::handle:horizontal:pressed {{
                background: {colors['scrollbar_thumb_pressed']};
            }}
            QTextEdit QScrollBar::add-line:horizontal,
            QTextEdit QScrollBar::sub-line:horizontal {{
                width: 0px;
                background: none;
                border: none;
            }}
            QTextEdit QScrollBar::add-page:horizontal,
            QTextEdit QScrollBar::sub-page:horizontal {{
                background: transparent;
            }}
            QMenu {{
                background-color: {colors['content_bg']};
                border: 1px solid {colors['border']};
                border-radius: 6px;
                padding: 5px;
            }}
            QMenu::item {{
                padding: 6px 25px 6px 15px;
                border-radius: 4px;
                color: {colors['text']};
            }}
            QMenu::item:selected {{
                background-color: {colors['menu_selected_bg']};
                color: {colors['menu_selected_color']};
            }}
            QComboBox {{
                padding: 2px 8px;
                border: none;
                border-radius: 5px;
                background: transparent;
                font-size: 12px;
                color: {colors['text']};
            }}
            QComboBox:hover {{
                background: {colors['hover_bg']};
            }}
            QComboBox:on {{
                background: {colors['pressed_bg']};
            }}
            QComboBox::drop-down {{
                border: none;
                width: 0px;
            }}
            QComboBox::down-arrow {{
                image: none;
                width: 0px;
            }}
            QComboBox QAbstractItemView {{
                background-color: {colors['content_bg']};
                border: 1px solid {colors['border']};
                border-radius: 5px;
                selection-background-color: {colors['menu_selected_bg']};
                selection-color: {colors['menu_selected_color']};
            }}
            QSplitter::handle {{
                background-color: transparent;
                height: 8px;
            }}
            QSplitter::handle:hover {{
                background-color: {colors['border']};
            }}
        """

    def get_toggle_button_active_style(self) -> str:
        """Get style for active toggle button (e.g., 'always on top')."""
        colors = self.get_colors()
        text_color = colors['primary'] if self.is_dark_theme() else colors['menu_selected_color']
        return f"""
            QPushButton {{
                background-color: {colors['toggle_active_bg']};
                border: none;
                border-radius: 6px;
                color: {text_color};
                font-size: 13px;
            }}
            QPushButton:hover {{
                background-color: {colors['toggle_active_hover_bg']};
            }}
        """

    def get_line_edit_style(self, error: bool = False) -> str:
        """返回 QLineEdit 样式表（统一来源，避免各窗口重复硬编码）。

        Args:
            error: True 时使用红色错误边框（用于热键冲突提示），
                   False 时使用常规边框并附带聚焦高亮规则。
        """
        colors = self.get_colors()
        border = colors['status_red_border'] if error else colors['border']
        style = f"""
            QLineEdit {{
                background-color: {colors['content_bg']};
                border: 1px solid {border};
                border-radius: 6px;
                padding: 6px 12px;
                font-size: 13px;
                color: {colors['text']};
            }}
        """
        if not error:
            style += f"""
            QLineEdit:focus {{
                border: 1px solid {colors['primary']};
            }}
            """
        return style

    def get_status_label_style(self, error: bool = False) -> str:
        """返回状态小标签样式表（绿色「已注册」/ 红色「已占用」）。"""
        colors = self.get_colors()
        if error:
            bg, border, fg = colors['status_red_bg'], colors['status_red_border'], colors['status_red']
        else:
            bg, border, fg = colors['status_green_bg'], colors['status_green'], colors['status_green']
        return f"""
            QLabel {{
                background-color: {bg};
                border: 1px solid {border};
                border-radius: 4px;
                color: {fg};
                font-size: 11px;
                padding: 2px 8px;
            }}
        """

    def get_status_button_style(self, is_active: bool) -> str:
        """Get style for status button (enabled/disabled state)."""
        colors = self.get_colors()
        if is_active:
            return f"""
                QPushButton {{
                    background-color: {colors['status_green_bg']};
                    border: 1px solid {colors['status_green']};
                    border-radius: 6px;
                    color: {colors['status_green']};
                    font-size: 12px;
                }}
                QPushButton:hover {{
                    background-color: {colors['status_green_bg']};
                }}
            """
        else:
            return f"""
                QPushButton {{
                    background-color: {colors['content_bg']};
                    border: 1px solid {colors['border']};
                    border-radius: 6px;
                    color: {colors['text_light']};
                    font-size: 12px;
                }}
                QPushButton:hover {{
                    background-color: {colors['hover_bg']};
                }}
            """
