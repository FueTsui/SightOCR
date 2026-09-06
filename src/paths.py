# paths.py
# Bootstrap module for custom config/log paths.
# Uses only stdlib so it's safe for early import before config/logging init.

import json
import os
import sys

# Determine ROOT_DIR (same logic as config.py)
if getattr(sys, 'frozen', False):
    ROOT_DIR = os.path.dirname(sys.executable)
else:
    ROOT_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

PATHS_FILE = os.path.join(ROOT_DIR, "paths.json")


def load_paths():
    """Read paths.json and return dict, or empty dict if missing/corrupt."""
    try:
        if os.path.exists(PATHS_FILE):
            with open(PATHS_FILE, 'r', encoding='utf-8') as f:
                return json.load(f)
    except Exception:
        pass
    return {}



def get_root_dir():
    """Return the application root directory."""
    return ROOT_DIR


def get_default_config_dir():
    """Return the default config directory (ROOT_DIR)."""
    return ROOT_DIR


def get_default_log_dir():
    """Return the default log directory."""
    if getattr(sys, 'frozen', False):
        return os.path.join(os.path.expanduser('~'), 'AppData', 'Roaming', 'SightOCR', 'logs')
    else:
        return os.path.join(ROOT_DIR, 'logs')


def get_config_dir():
    """Return the custom config directory, or default if not set."""
    paths = load_paths()
    return paths.get('config_dir', get_default_config_dir())


def get_config_file():
    """Return the full path to config.json."""
    return os.path.join(get_config_dir(), 'config.json')


def get_log_dir():
    """Return the custom log directory, or default if not set."""
    paths = load_paths()
    return paths.get('log_dir', get_default_log_dir())
