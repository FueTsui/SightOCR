# autostart.py
import sys
import os
import winreg
from config import load_config

APP_NAME = "SightOCR"
APP_PATH = os.path.realpath(sys.argv[0])

def set_autostart(enable):
    registry_path = r"Software\Microsoft\Windows\CurrentVersion\Run"
    key = winreg.OpenKey(winreg.HKEY_CURRENT_USER, registry_path, 0, winreg.KEY_WRITE)

    try:
        if enable:
            command = f'"{APP_PATH}" --silent'
            winreg.SetValueEx(key, APP_NAME, 0, winreg.REG_SZ, command)
        else:
            winreg.DeleteValue(key, APP_NAME)
    except FileNotFoundError:
        pass
    finally:
        winreg.CloseKey(key)

def is_autostart_enabled():
    registry_path = r"Software\Microsoft\Windows\CurrentVersion\Run"
    try:
        key = winreg.OpenKey(winreg.HKEY_CURRENT_USER, registry_path, 0, winreg.KEY_READ)
        winreg.QueryValueEx(key, APP_NAME)
        winreg.CloseKey(key)
        return True
    except FileNotFoundError:
        return False

if __name__ == "__main__":
    # 载入配置并初始化设置
    config_data = load_config()
