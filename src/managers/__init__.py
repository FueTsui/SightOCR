# managers/__init__.py
"""
Manager classes for SightOCR application.
Provides centralized management for themes, configuration, tray, OCR, and translation.
"""

from .theme_manager import ThemeManager
from .config_manager import ConfigManager
from .tray_manager import TrayManager
from .ocr_manager import OCRManager
from .translation_manager import TranslationManager

__all__ = [
    'ThemeManager',
    'ConfigManager',
    'TrayManager',
    'OCRManager',
    'TranslationManager',
]
