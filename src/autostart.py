# autostart.py

import sys
import os
import winreg
import logging

APP_NAME = "SightOCR"


def get_app_path():
    """获取应用程序路径"""
    if getattr(sys, 'frozen', False):
        # PyInstaller 打包后的可执行文件
        return sys.executable
    else:
        # 开发环境
        return os.path.realpath(sys.argv[0])


def set_autostart(enable):
    """设置开机自启"""
    registry_path = r"Software\Microsoft\Windows\CurrentVersion\Run"
    key = None

    try:
        key = winreg.OpenKey(winreg.HKEY_CURRENT_USER, registry_path, 0, winreg.KEY_WRITE)

        if enable:
            app_path = get_app_path()
            command = f'"{app_path}" --silent'
            winreg.SetValueEx(key, APP_NAME, 0, winreg.REG_SZ, command)
            logging.info(f"已启用开机自启: {command}")
        else:
            try:
                winreg.DeleteValue(key, APP_NAME)
                logging.info("已禁用开机自启")
            except FileNotFoundError:
                # 注册表项不存在，无需删除
                pass

        return True

    except PermissionError as e:
        logging.error(f"设置开机自启失败（权限错误）: {e}")
        return False
    except OSError as e:
        logging.error(f"设置开机自启失败（系统错误）: {e}")
        return False
    except Exception as e:
        logging.error(f"设置开机自启失败: {e}")
        return False
    finally:
        if key:
            try:
                winreg.CloseKey(key)
            except Exception:
                pass


def is_autostart_enabled():
    """检查是否已启用开机自启"""
    registry_path = r"Software\Microsoft\Windows\CurrentVersion\Run"
    key = None

    try:
        key = winreg.OpenKey(winreg.HKEY_CURRENT_USER, registry_path, 0, winreg.KEY_READ)
        value, _ = winreg.QueryValueEx(key, APP_NAME)
        return bool(value)
    except FileNotFoundError:
        return False
    except PermissionError:
        logging.debug("检查开机自启状态失败（权限错误）")
        return False
    except Exception as e:
        logging.debug(f"检查开机自启状态失败: {e}")
        return False
    finally:
        if key:
            try:
                winreg.CloseKey(key)
            except Exception:
                pass
