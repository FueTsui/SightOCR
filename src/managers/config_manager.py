# config_manager.py
"""
Singleton ConfigManager for centralized configuration access.
Replaces scattered load_config/save_config calls with a unified interface.
"""

import logging
import threading
from typing import Any, Optional
from PySide6.QtCore import QObject, Signal

from ..config import load_config, save_config, DEFAULT_CONFIG


class ConfigManager(QObject):
    """
    Singleton manager for application configuration.

    Usage:
        config_mgr = ConfigManager.instance()
        value = config_mgr.get('hotkey', 'F4')
        config_mgr.set('hotkey', 'F5')
        config_mgr.save()

        # Batch updates
        config_mgr.update({'hotkey': 'F5', 'translate_hotkey': 'F3'})
    """

    # Signal emitted when config is saved
    config_saved = Signal()

    # Signal emitted when a specific key changes (key: str, value: Any)
    config_changed = Signal(str, object)

    _instance = None
    _initialized = False

    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def __init__(self):
        if ConfigManager._initialized:
            return
        super().__init__()
        ConfigManager._initialized = True

        # 可重入锁保护内存层 _config / _dirty 的读改写。
        # 后台线程（auto_update 的每日检查）与主线程会并发访问，
        # 没有锁时 set→save 的读改写序列存在竞态（A1）。
        # 用 RLock 以支持 set→save、update→set→save 等同线程重入调用。
        self._lock = threading.RLock()
        self._config = {}
        self._dirty = False
        self._load()

        logging.debug("ConfigManager initialized")

    @classmethod
    def instance(cls) -> 'ConfigManager':
        """Get the singleton instance."""
        if cls._instance is None:
            cls._instance = cls()
        return cls._instance

    def _load(self):
        """Load configuration from file."""
        loaded = load_config()
        with self._lock:
            self._config = loaded
            self._dirty = False
        logging.debug("Configuration loaded")

    def reload(self):
        """Reload configuration from file, discarding unsaved changes."""
        self._load()

    def get(self, key: str, default: Any = None) -> Any:
        """
        Get a configuration value.

        Args:
            key: Configuration key
            default: Default value if key doesn't exist

        Returns:
            The configuration value or default
        """
        with self._lock:
            return self._config.get(key, default)

    def set(self, key: str, value: Any, auto_save: bool = False):
        """
        Set a configuration value.

        Args:
            key: Configuration key
            value: Configuration value
            auto_save: If True, save immediately after setting
        """
        with self._lock:
            old_value = self._config.get(key)
            changed = old_value != value
            if changed:
                self._config[key] = value
                self._dirty = True

        # 信号在锁外发射，避免在持锁状态下执行任意槽函数（可能回调本管理器）
        if changed:
            self.config_changed.emit(key, value)
            logging.debug(f"Config '{key}' changed from {old_value} to {value}")
            if auto_save:
                self.save()

    def remove(self, key: str, auto_save: bool = False):
        """
        Remove a configuration key if present.

        Args:
            key: Configuration key to remove
            auto_save: If True, save immediately after removing
        """
        with self._lock:
            existed = key in self._config
            if existed:
                del self._config[key]
                self._dirty = True

        if existed:
            logging.debug(f"Config '{key}' removed")
            if auto_save:
                self.save()

    def update(self, updates: dict, auto_save: bool = True):
        """
        Update multiple configuration values.

        Args:
            updates: Dictionary of key-value pairs to update
            auto_save: If True, save after all updates (default: True)
        """
        for key, value in updates.items():
            self.set(key, value, auto_save=False)

        with self._lock:
            need_save = auto_save and self._dirty
        if need_save:
            self.save()

    def save(self) -> bool:
        """
        Save configuration to file.

        Returns:
            True if save was successful, False otherwise
        """
        # 在锁内快照，确保写盘期间 _config 不被其他线程改动；
        # 实际文件写入（save_config）在锁外执行，避免持锁进行磁盘 IO。
        with self._lock:
            snapshot = self._config.copy()

        result = save_config(snapshot)
        if result:
            with self._lock:
                self._dirty = False
            self.config_saved.emit()
            logging.debug("Configuration saved")
        else:
            logging.error("Failed to save configuration")
        return result

    def is_dirty(self) -> bool:
        """Return True if there are unsaved changes."""
        with self._lock:
            return self._dirty

    def get_all(self) -> dict:
        """Return a copy of all configuration."""
        with self._lock:
            return self._config.copy()

    def reset_to_defaults(self, save_immediately: bool = True):
        """Reset configuration to defaults."""
        with self._lock:
            self._config = DEFAULT_CONFIG.copy()
            self._dirty = True
        if save_immediately:
            self.save()
        logging.info("Configuration reset to defaults")

    # ==================== Convenience Properties ====================

    @property
    def api_key(self) -> str:
        """Baidu OCR API key."""
        return self.get('api_key', '')

    @api_key.setter
    def api_key(self, value: str):
        self.set('api_key', value)

    @property
    def secret_key(self) -> str:
        """Baidu OCR secret key."""
        return self.get('secret_key', '')

    @secret_key.setter
    def secret_key(self, value: str):
        self.set('secret_key', value)

    @property
    def baidu_trans_appid(self) -> str:
        """Baidu translate APP ID."""
        return self.get('baidu_trans_appid', '')

    @baidu_trans_appid.setter
    def baidu_trans_appid(self, value: str):
        self.set('baidu_trans_appid', value)

    @property
    def baidu_trans_appkey(self) -> str:
        """Baidu translate APP key."""
        return self.get('baidu_trans_appkey', '')

    @baidu_trans_appkey.setter
    def baidu_trans_appkey(self, value: str):
        self.set('baidu_trans_appkey', value)

    @property
    def tencent_secret_id(self) -> str:
        """Tencent OCR secret ID."""
        return self.get('tencent_secret_id', '')

    @tencent_secret_id.setter
    def tencent_secret_id(self, value: str):
        self.set('tencent_secret_id', value)

    @property
    def tencent_secret_key(self) -> str:
        """Tencent OCR secret key."""
        return self.get('tencent_secret_key', '')

    @tencent_secret_key.setter
    def tencent_secret_key(self, value: str):
        self.set('tencent_secret_key', value)

    @property
    def tencent_trans_secret_id(self) -> str:
        """Tencent translate secret ID."""
        return self.get('tencent_trans_secret_id', '')

    @tencent_trans_secret_id.setter
    def tencent_trans_secret_id(self, value: str):
        self.set('tencent_trans_secret_id', value)

    @property
    def tencent_trans_secret_key(self) -> str:
        """Tencent translate secret key."""
        return self.get('tencent_trans_secret_key', '')

    @tencent_trans_secret_key.setter
    def tencent_trans_secret_key(self, value: str):
        self.set('tencent_trans_secret_key', value)

    @property
    def hotkey(self) -> str:
        """OCR hotkey."""
        return self.get('hotkey', 'F4')

    @hotkey.setter
    def hotkey(self, value: str):
        self.set('hotkey', value)

    @property
    def translate_hotkey(self) -> str:
        """Translate hotkey."""
        return self.get('translate_hotkey', 'F2')

    @translate_hotkey.setter
    def translate_hotkey(self, value: str):
        self.set('translate_hotkey', value)

    @property
    def hide_tray_icon(self) -> bool:
        """Whether to hide tray icon."""
        return self.get('hide_tray_icon', False)

    @hide_tray_icon.setter
    def hide_tray_icon(self, value: bool):
        self.set('hide_tray_icon', value)

    @property
    def replace_newline(self) -> bool:
        """Whether to replace newlines with spaces in OCR text results."""
        return self.get('replace_newline', False)

    @replace_newline.setter
    def replace_newline(self, value: bool):
        self.set('replace_newline', value)

    @property
    def last_ocr_selection(self) -> str:
        """Last selected OCR source."""
        return self.get('last_ocr_selection', '默认')

    @last_ocr_selection.setter
    def last_ocr_selection(self, value: str):
        self.set('last_ocr_selection', value)

    @property
    def last_translate_selection(self) -> str:
        """Last selected translation source."""
        return self.get('last_translate_selection', '默认')

    @last_translate_selection.setter
    def last_translate_selection(self, value: str):
        self.set('last_translate_selection', value)

    @property
    def last_source_lang(self) -> str:
        """Last selected source language."""
        return self.get('last_source_lang', '自动识别')

    @last_source_lang.setter
    def last_source_lang(self, value: str):
        self.set('last_source_lang', value)

    @property
    def last_target_lang(self) -> str:
        """Last selected target language."""
        return self.get('last_target_lang', '简体中文')

    @last_target_lang.setter
    def last_target_lang(self, value: str):
        self.set('last_target_lang', value)

    # ==================== API Credential Helpers ====================

    def has_baidu_ocr_credentials(self) -> bool:
        """Check if Baidu OCR credentials are configured."""
        return bool(self.api_key and self.secret_key)

    def has_baidu_trans_credentials(self) -> bool:
        """Check if Baidu translate credentials are configured."""
        return bool(self.baidu_trans_appid and self.baidu_trans_appkey)

    def has_tencent_ocr_credentials(self) -> bool:
        """Check if Tencent OCR credentials are configured."""
        return bool(self.tencent_secret_id and self.tencent_secret_key)

    def has_tencent_trans_credentials(self) -> bool:
        """Check if Tencent translate credentials are configured."""
        return bool(self.tencent_trans_secret_id and self.tencent_trans_secret_key)
