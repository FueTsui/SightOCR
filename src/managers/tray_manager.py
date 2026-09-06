# tray_manager.py
"""
TrayManager for system tray icon management.
Extracted from MainWindow to reduce its size.
"""

import logging
from typing import Callable, Optional

from PySide6.QtWidgets import QSystemTrayIcon, QMenu
from PySide6.QtGui import QIcon, QAction
from PySide6.QtCore import QObject, Signal, QTimer

from ..utils import resource_path

try:
    from shiboken6 import isValid as _isValid
except ImportError:
    def _isValid(obj):
        """Fallback when shiboken6 is not available."""
        try:
            obj.objectName()
            return True
        except RuntimeError:
            return False


class TrayManager(QObject):
    """
    Manages the system tray icon and its context menu.

    Usage:
        tray_mgr = TrayManager(parent_window)
        tray_mgr.set_callbacks(
            on_show=window.show,
            on_ocr=window.start_ocr,
            on_translate=window.start_translate,
            on_settings=window.open_settings,
            on_restart=window.restart,
            on_quit=window.quit
        )
        tray_mgr.show()
    """

    # Emitted when tray icon is activated
    activated = Signal(QSystemTrayIcon.ActivationReason)

    def __init__(self, parent=None):
        super().__init__(parent)

        self._parent = parent
        self._tray_icon = None
        self._menu = None
        self._visible = False
        self._cleaned_up = False

        # Menu actions (stored for updating labels)
        self._ocr_action = None
        self._translate_action = None

        # Hotkey labels
        self._ocr_hotkey = 'F4'
        self._translate_hotkey = 'F2'

        # Callbacks
        self._on_show = None
        self._on_ocr = None
        self._on_translate = None
        self._on_settings = None
        self._on_restart = None
        self._on_quit = None

        # Debounce timer for single-click vs double-click
        self._click_timer = None

        self._init_tray_icon()

    def _is_parent_valid(self) -> bool:
        """Check if the parent window's C++ object is still alive."""
        return self._parent is not None and _isValid(self._parent)

    def _init_tray_icon(self):
        """Initialize the system tray icon and menu."""
        icon_path = resource_path("assets/icon.png")
        self._tray_icon = QSystemTrayIcon(QIcon(icon_path), parent=self._parent)
        self._tray_icon.setToolTip("SightOCR")
        self._tray_icon.activated.connect(self._on_tray_activated)

        # Create menu
        self._menu = QMenu(self._parent)
        self._create_menu_actions()
        self._tray_icon.setContextMenu(self._menu)

        # Debounce timer: delays single-click so double-click can cancel it
        self._click_timer = QTimer(self)
        self._click_timer.setSingleShot(True)
        self._click_timer.setInterval(250)
        self._click_timer.timeout.connect(self._handle_single_click)

        logging.debug("TrayManager initialized")

    def _create_menu_actions(self):
        """Create the context menu actions."""
        # Show main window
        show_action = QAction("主界面", self._parent)
        show_action.triggered.connect(lambda: self._safe_call(self._on_show))
        self._menu.addAction(show_action)

        # OCR
        self._ocr_action = QAction(f"识别    {self._ocr_hotkey}", self._parent)
        self._ocr_action.triggered.connect(lambda: self._safe_call(self._on_ocr))
        self._menu.addAction(self._ocr_action)

        # Translate
        self._translate_action = QAction(f"翻译    {self._translate_hotkey}", self._parent)
        self._translate_action.triggered.connect(lambda: self._safe_call(self._on_translate))
        self._menu.addAction(self._translate_action)

        # Settings
        settings_action = QAction("设置", self._parent)
        settings_action.triggered.connect(lambda: self._safe_call(self._on_settings))
        self._menu.addAction(settings_action)

        # Restart
        restart_action = QAction("重启", self._parent)
        restart_action.triggered.connect(lambda: self._safe_call(self._on_restart))
        self._menu.addAction(restart_action)

        # Quit
        quit_action = QAction("退出", self._parent)
        quit_action.triggered.connect(lambda: self._safe_call(self._on_quit))
        self._menu.addAction(quit_action)

    def _safe_call(self, callback: Optional[Callable]):
        """Safely call a callback if it exists."""
        if self._cleaned_up:
            return
        if not self._is_parent_valid():
            return
        if callback:
            try:
                callback()
            except RuntimeError as e:
                # C++ object deleted — expected during shutdown
                logging.debug(f"Tray callback RuntimeError (object deleted?): {e}")
            except Exception as e:
                logging.error(f"Tray callback error: {e}", exc_info=True)

    def _on_tray_activated(self, reason: QSystemTrayIcon.ActivationReason):
        """Handle tray icon activation."""
        if self._cleaned_up:
            return

        self.activated.emit(reason)

        if reason == QSystemTrayIcon.ActivationReason.Trigger:
            # Defer single-click so a following double-click can cancel it.
            # This prevents hide→show flickering on double-click.
            if self._click_timer:
                self._click_timer.start()
        elif reason == QSystemTrayIcon.ActivationReason.DoubleClick:
            # Cancel any pending single-click action
            if self._click_timer:
                self._click_timer.stop()
            self._show_parent_window()

    def _handle_single_click(self):
        """Execute the actual single-click toggle after debounce period."""
        if self._cleaned_up:
            return
        if not self._is_parent_valid():
            return
        try:
            if self._parent.isVisible() and not self._parent.isMinimized():
                self._parent.hide()
            else:
                self._parent.showNormal()
                self._parent.activateWindow()
        except RuntimeError:
            logging.debug("Parent C++ object deleted during single-click handling")

    def _show_parent_window(self):
        """Safely show and activate the parent window."""
        if not self._is_parent_valid():
            return
        try:
            self._parent.showNormal()
            self._parent.activateWindow()
        except RuntimeError:
            logging.debug("Parent C++ object deleted during show")

    def set_callbacks(
        self,
        on_show: Optional[Callable] = None,
        on_ocr: Optional[Callable] = None,
        on_translate: Optional[Callable] = None,
        on_settings: Optional[Callable] = None,
        on_restart: Optional[Callable] = None,
        on_quit: Optional[Callable] = None
    ):
        """
        Set callback functions for menu actions.

        Args:
            on_show: Called when "主界面" is clicked
            on_ocr: Called when "识别" is clicked
            on_translate: Called when "翻译" is clicked
            on_settings: Called when "设置" is clicked
            on_restart: Called when "重启" is clicked
            on_quit: Called when "退出" is clicked
        """
        self._on_show = on_show
        self._on_ocr = on_ocr
        self._on_translate = on_translate
        self._on_settings = on_settings
        self._on_restart = on_restart
        self._on_quit = on_quit

    def update_hotkey_labels(self, ocr_hotkey: str, translate_hotkey: str):
        """
        Update the hotkey labels in the menu.

        Args:
            ocr_hotkey: OCR hotkey string (e.g., 'F4')
            translate_hotkey: Translate hotkey string (e.g., 'F2')
        """
        if self._cleaned_up:
            return

        self._ocr_hotkey = ocr_hotkey
        self._translate_hotkey = translate_hotkey

        try:
            if self._ocr_action:
                self._ocr_action.setText(f"识别    {ocr_hotkey}")
            if self._translate_action:
                self._translate_action.setText(f"翻译    {translate_hotkey}")
        except RuntimeError:
            logging.debug("Menu action C++ object deleted during hotkey label update")
            return

        logging.debug(f"Tray hotkey labels updated: OCR={ocr_hotkey}, Translate={translate_hotkey}")

    def show(self):
        """Show the tray icon."""
        if self._tray_icon and not self._cleaned_up:
            try:
                self._tray_icon.show()
                self._visible = True
                logging.debug("Tray icon shown")
            except RuntimeError:
                logging.debug("Tray icon C++ object deleted during show")

    def hide(self):
        """Hide the tray icon."""
        if self._tray_icon and not self._cleaned_up:
            try:
                self._tray_icon.hide()
                self._visible = False
                logging.debug("Tray icon hidden")
            except RuntimeError:
                logging.debug("Tray icon C++ object deleted during hide")

    def set_visible(self, visible: bool):
        """Set tray icon visibility."""
        if visible:
            self.show()
        else:
            self.hide()

    def is_visible(self) -> bool:
        """Return whether the tray icon is visible."""
        return self._visible

    def show_message(self, title: str, message: str,
                     icon: QSystemTrayIcon.MessageIcon = QSystemTrayIcon.Information,
                     duration_ms: int = 5000):
        """
        Show a tray notification message.

        Args:
            title: Message title
            message: Message body
            icon: Message icon type
            duration_ms: Display duration in milliseconds
        """
        if self._tray_icon and self._visible and not self._cleaned_up:
            try:
                self._tray_icon.showMessage(title, message, icon, duration_ms)
            except RuntimeError:
                pass

    def cleanup(self):
        """Clean up tray icon before application exit."""
        if self._cleaned_up:
            return
        self._cleaned_up = True

        # Stop debounce timer to prevent pending callbacks
        if self._click_timer:
            self._click_timer.stop()

        if self._tray_icon:
            try:
                self._tray_icon.hide()
            except RuntimeError:
                pass
            self._tray_icon = None
        logging.debug("TrayManager cleaned up")
