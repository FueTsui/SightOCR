# utils.py

import sys
import os
import ctypes
import logging
import subprocess

def resource_path(relative_path):
    """ 获取资源的绝对路径，适用于开发环境和PyInstaller打包环境 """
    if hasattr(sys, 'frozen') and hasattr(sys, '_MEIPASS'):
        # PyInstaller 创建一个临时文件夹，并将路径存储在 _MEIPASS 中
        base_path = sys._MEIPASS
        logging.debug(f"Resource Path: frozen mode, base_path={base_path}") # Added logging
    else:
        # 未打包状态，使用主脚本的目录
        base_path = os.path.dirname(os.path.abspath(sys.argv[0]))
        logging.debug(f"Resource Path: script mode, base_path={base_path}") # Added logging
    full_path = os.path.join(base_path, relative_path)
    logging.debug(f"Resource Path: relative_path='{relative_path}', full_path='{full_path}'") # Added logging
    return full_path

def disable_dpi_scaling():
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)
    except Exception as e:
        logging.warning(f"设置 DPI 感知失败: {e}")

def trim_working_set():
    """将进程工作集交还给系统，降低后台驻留时的物理内存占用。

    被换出的页面进入系统备用列表（standby list），下次访问时以软缺页
    （soft page fault，微秒级）取回，不涉及磁盘 IO，对热键响应速度的
    影响可忽略。适合在窗口隐藏到托盘后调用。
    """
    try:
        from ctypes import wintypes
        # 必须显式声明 HANDLE 类型：64 位下 GetCurrentProcess 返回的伪句柄
        # (-1) 若按默认 32 位 int 传递会被截断，导致调用失败
        kernel32 = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel32.GetCurrentProcess.restype = wintypes.HANDLE
        kernel32.K32EmptyWorkingSet.argtypes = [wintypes.HANDLE]
        kernel32.K32EmptyWorkingSet.restype = wintypes.BOOL
        result = kernel32.K32EmptyWorkingSet(kernel32.GetCurrentProcess())
        if not result:
            logging.debug(f"收缩工作集失败，错误码: {ctypes.get_last_error()}")
        return bool(result)
    except Exception as e:
        logging.debug(f"收缩工作集失败: {e}")
        return False

def run_as_admin():
    if ctypes.windll.shell32.IsUserAnAdmin():
        return True
    else:
        # 如果不是管理员，重新启动并请求管理员权限。
        # 使用 subprocess.list2cmdline 对参数正确加引号，避免路径/参数含空格时被错误拆分。
        # 打包环境下 sys.executable 即程序本身，参数不含脚本路径；
        # 开发环境下需把脚本路径(argv[0])作为第一个参数传给 python.exe。
        if getattr(sys, 'frozen', False):
            params = subprocess.list2cmdline(sys.argv[1:])
        else:
            params = subprocess.list2cmdline(sys.argv)
        ctypes.windll.shell32.ShellExecuteW(None, "runas", sys.executable, params, None, 1)
        return False
