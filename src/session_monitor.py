# session_monitor.py

import ctypes
from ctypes import wintypes
import win32con
import win32api
import win32gui
import logging
import threading

# Windows 会话变更消息常量
WTS_SESSION_UNLOCK = 0x8
WTS_SESSION_LOCK = 0x7
WM_WTSSESSION_CHANGE = 0x02B1

# 会话通知类型
NOTIFY_FOR_THIS_SESSION = 0


class SessionMonitor:
    """监控 Windows 会话锁定/解锁事件"""

    def __init__(self, hotkey_handler):
        self.hotkey_handler = hotkey_handler
        self.monitor_thread = None
        self._running = False
        self._hwnd = None
        self._thread_id = None
        self._wnd_proc_ref = None  # 保持回调函数引用，防止 GC
        self._window_class_name = "SightOCRSessionMonitorWindow"

    def start_session_monitor_thread(self):
        """启动会话监控线程"""
        if self.is_alive():
            logging.debug("会话监控线程已在运行")
            return True

        if self.monitor_thread and self.monitor_thread.is_alive():
            logging.warning("会话监控线程状态异常，正在尝试重建")
            self.stop()
            self.monitor_thread.join(timeout=2.0)
            if self.monitor_thread and self.monitor_thread.is_alive():
                logging.error("会话监控线程未能及时退出，取消本次重建")
                return False

        self._running = True
        self.monitor_thread = threading.Thread(
            target=self._session_monitor_loop,
            daemon=True,
            name="SessionMonitor"
        )
        self.monitor_thread.start()
        logging.info("会话监控线程已启动")
        return True

    def is_alive(self) -> bool:
        """返回会话监控线程和隐藏窗口是否都处于可用状态。"""
        thread = self.monitor_thread
        return bool(thread and thread.is_alive() and self._is_window_valid())

    def _is_window_valid(self) -> bool:
        if not self._hwnd:
            return False
        try:
            return bool(win32gui.IsWindow(self._hwnd))
        except Exception:
            return False

    def _create_wnd_proc(self):
        """创建窗口消息处理函数（保持引用防止 GC）"""
        def wnd_proc(hwnd, msg, wparam, lparam):
            try:
                if msg == WM_WTSSESSION_CHANGE:
                    if wparam == WTS_SESSION_UNLOCK:
                        logging.info("检测到会话解锁")
                        if self.hotkey_handler:
                            # 使用独立线程调用热键重注册，避免死锁
                            # 因为 re_register_hotkeys 会发送消息到热键线程
                            def async_reregister():
                                try:
                                    self.hotkey_handler.re_register_hotkeys()
                                    logging.info("快捷键已重新注册")
                                except Exception as e:
                                    logging.error(f"重新注册快捷键失败: {e}")
                            threading.Thread(
                                target=async_reregister,
                                daemon=True,
                                name="HotkeyReregister"
                            ).start()
                    elif wparam == WTS_SESSION_LOCK:
                        logging.info("检测到会话锁定")
            except Exception as e:
                logging.error(f"处理会话变更事件失败: {e}")
            return win32gui.DefWindowProc(hwnd, msg, wparam, lparam)
        return wnd_proc

    def _session_monitor_loop(self):
        """会话监控主循环"""
        hwnd = None
        wc = None

        # 创建并保持回调函数引用
        self._wnd_proc_ref = self._create_wnd_proc()
        self._thread_id = win32api.GetCurrentThreadId()

        # 创建窗口类
        wc = win32gui.WNDCLASS()
        wc.lpfnWndProc = self._wnd_proc_ref
        wc.lpszClassName = self._window_class_name

        try:
            try:
                class_atom = win32gui.RegisterClass(wc)
            except win32gui.error as e:
                # 窗口类可能已注册（错误码 1410）
                if e.winerror == 1410:
                    class_atom = wc.lpszClassName
                else:
                    raise

            # 创建隐藏窗口
            hwnd = win32gui.CreateWindow(
                class_atom,
                "SightOCR Session Monitor",
                0,
                0, 0, 0, 0,
                0, 0, 0, None
            )

            if not hwnd:
                logging.error("创建会话监控窗口失败")
                return

            self._hwnd = hwnd

            # 注册会话通知
            result = ctypes.windll.wtsapi32.WTSRegisterSessionNotification(
                hwnd, NOTIFY_FOR_THIS_SESSION
            )
            if not result:
                error_code = ctypes.get_last_error()
                logging.error(f"WTSRegisterSessionNotification 失败，错误码: {error_code}")
                win32gui.DestroyWindow(hwnd)
                return

            logging.debug("会话通知注册成功")

            # 消息循环 - 使用 GetMessage 阻塞等待，避免 CPU 空转
            while self._running:
                try:
                    # GetMessage 会阻塞直到有消息到达，避免 CPU 空转
                    msg = win32gui.GetMessage(None, 0, 0)
                    if msg[0]:
                        win32gui.TranslateMessage(msg[1])
                        win32gui.DispatchMessage(msg[1])
                    else:
                        # WM_QUIT
                        break
                except Exception as e:
                    logging.error(f"消息处理异常: {e}")
                    if not self._running:
                        break

        except Exception as e:
            logging.error(f"会话监控线程异常: {e}", exc_info=True)
        finally:
            # 清理资源
            if hwnd:
                try:
                    ctypes.windll.wtsapi32.WTSUnRegisterSessionNotification(hwnd)
                except Exception:
                    pass
                try:
                    win32gui.DestroyWindow(hwnd)
                except Exception:
                    pass

            if wc is not None:
                try:
                    win32gui.UnregisterClass(wc.lpszClassName, None)
                except Exception:
                    pass

            self._hwnd = None
            self._thread_id = None
            self._wnd_proc_ref = None
            self.monitor_thread = None
            logging.info("会话监控线程已结束")

    def stop(self):
        """停止会话监控"""
        self._running = False
        if self._thread_id:
            try:
                win32api.PostThreadMessage(self._thread_id, win32con.WM_QUIT, 0, 0)
                return
            except Exception as e:
                logging.debug(f"发送会话监控线程退出消息失败: {e}")
        if self._hwnd:
            try:
                win32gui.PostMessage(self._hwnd, win32con.WM_CLOSE, 0, 0)
            except Exception:
                pass
