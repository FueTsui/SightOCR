# hotkey_handler.py

import win32gui
import win32con
import win32api
import ctypes
from ctypes import windll, wintypes
import threading
from concurrent.futures import ThreadPoolExecutor
import logging
import time
from typing import Callable, Dict, List, Optional, Tuple

# 自定义消息，用于通知线程刷新热键配置
WM_REFRESH_HOTKEYS = win32con.WM_USER + 1

# 最大热键 ID（防止溢出）
MAX_HOTKEY_ID = 0xBFFF

# 热键错误码
ERROR_HOTKEY_ALREADY_REGISTERED = 1409
ERROR_INVALID_PARAMETER = 87

# 预定义的 fallback 热键列表
FALLBACK_HOTKEYS = {
    'OCR': ['F4', 'Ctrl+F4', 'Alt+O', 'Ctrl+Shift+O'],
    '翻译': ['F2', 'Ctrl+F2', 'Alt+T', 'Ctrl+Shift+T'],
}

# 错误码映射
ERROR_MESSAGES = {
    0: "已占用",
    ERROR_HOTKEY_ALREADY_REGISTERED: "已占用",
    ERROR_INVALID_PARAMETER: "已占用",
}

WNDPROC = ctypes.WINFUNCTYPE(
    wintypes.LPARAM,
    wintypes.HWND,
    wintypes.UINT,
    wintypes.WPARAM,
    wintypes.LPARAM,
)


class HotkeyHandler:
    def __init__(self):
        self.hwnd = None
        self.hotkey_callbacks = {}
        self.next_hotkey_id = 1
        self.registered_hotkeys_by_name = {}
        self.hotkey_id_to_name = {}  # 反向映射：hotkey_id -> name，优化查找效率
        self.hotkey_configs = {}
        self.last_trigger_time = {}  # 防抖
        self.init_event = threading.Event()  # 用于等待线程初始化完成
        self._running = True
        self._lock = threading.Lock()  # 保护配置数据和防抖时间
        self._wndproc_wrapper = None  # 保持 WNDPROC 回调引用，防止 GC
        self._listener_thread = None
        self._thread_id = None
        self._window_class_name = "HotkeyHandlerWindow"
        # 使用线程池限制并发回调数量，防止线程爆炸
        self._callback_executor = ThreadPoolExecutor(max_workers=2, thread_name_prefix="HotkeyCallback")

    def start_hotkey_listener(self):
        """启动热键监听线程，并等待窗口创建完成"""
        if self.is_listener_alive():
            return True

        if self._listener_thread and self._listener_thread.is_alive():
            logging.warning("热键监听线程状态异常，正在尝试重建")
            self._request_stop(join_timeout=2.0)
            if self._listener_thread and self._listener_thread.is_alive():
                logging.error("热键监听线程未能及时退出，取消本次重建")
                return False

        self.init_event = threading.Event()
        self._running = True
        self.hwnd = None
        self._thread_id = None
        self._listener_thread = threading.Thread(
            target=self._thread_entry,
            daemon=True,
            name="HotkeyListener"
        )
        self._listener_thread.start()
        # 等待子线程创建窗口句柄
        if not self.init_event.wait(timeout=5):
            logging.error("热键监听线程初始化超时！")
            return False
        return self.hwnd is not None

    def _thread_entry(self):
        """热键线程入口：创建窗口并启动消息循环"""
        wc = win32gui.WNDCLASS()
        # 保存 WNDPROC 包装器引用，防止被 GC 回收导致崩溃
        self._wndproc_wrapper = WNDPROC(self._wnd_proc)
        wc.lpfnWndProc = self._wndproc_wrapper
        wc.lpszClassName = self._window_class_name
        self._thread_id = win32api.GetCurrentThreadId()

        try:
            try:
                win32gui.RegisterClass(wc)
            except win32gui.error as e:
                # 类可能已经注册
                if e.winerror != 1410:  # ERROR_CLASS_ALREADY_EXISTS
                    raise

            # CreateWindow 必须在这个线程内调用
            self.hwnd = win32gui.CreateWindow(
                wc.lpszClassName, "Hotkey Handler Window", 0,
                0, 0, 0, 0, 0, 0, 0, None
            )
            logging.info(f"热键窗口已创建，句柄: {self.hwnd}")
        except Exception as e:
            logging.error(f"创建热键窗口失败: {e}")
            self.hwnd = None
        finally:
            # 通知主线程初始化完成
            self.init_event.set()

        try:
            if self.hwnd:
                self._message_loop()
        finally:
            self._cleanup_native_resources()

    def _wnd_proc(self, hwnd, message, wparam, lparam):
        """窗口过程函数"""
        try:
            # 处理热键触发
            if message == win32con.WM_HOTKEY:
                hotkey_id = wparam
                callback = self.hotkey_callbacks.get(hotkey_id)
                if callback:
                    # 使用反向映射快速查找热键名称，O(1) 复杂度
                    with self._lock:
                        hotkey_name = self.hotkey_id_to_name.get(hotkey_id)
                        if hotkey_name:
                            now = time.time()
                            last_time = self.last_trigger_time.get(hotkey_name, 0)
                            if now - last_time > 0.5:  # 防抖
                                self.last_trigger_time[hotkey_name] = now
                                # 使用线程池执行回调，避免阻塞消息循环且防止线程爆炸
                                try:
                                    self._callback_executor.submit(self._safe_callback, callback, hotkey_name)
                                except RuntimeError:
                                    # 线程池已关闭
                                    logging.warning(f"热键回调线程池已关闭，无法执行回调: {hotkey_name}")

            # 处理刷新请求
            elif message == WM_REFRESH_HOTKEYS:
                logging.debug("接收到刷新热键指令，正在执行重注册...")
                self._perform_registration_in_thread()

        except Exception as e:
            logging.error(f"处理窗口消息时发生异常: {e}", exc_info=True)

        return win32gui.DefWindowProc(hwnd, message, wparam, lparam)

    def _safe_callback(self, callback, hotkey_name):
        """安全地执行回调函数"""
        try:
            callback()
        except Exception as e:
            logging.error(f"热键 '{hotkey_name}' 回调执行失败: {e}", exc_info=True)

    def _message_loop(self):
        """标准 Windows 消息循环"""
        while self._running:
            try:
                # GetMessage 会阻塞直到有消息
                msg = win32gui.GetMessage(None, 0, 0)
                if msg[0]:
                    win32gui.TranslateMessage(msg[1])
                    win32gui.DispatchMessage(msg[1])
                else:
                    break  # WM_QUIT
            except Exception as e:
                logging.error(f"消息循环错误: {e}")
                # 短暂休眠后继续，避免 CPU 占用过高
                time.sleep(0.1)
                if not self._running:
                    break

    def _cleanup_native_resources(self):
        """在监听线程退出时释放本地窗口和热键资源。"""
        hwnd = self.hwnd
        with self._lock:
            registered_hotkeys = list(self.registered_hotkeys_by_name.values())
            self.registered_hotkeys_by_name.clear()
            self.hotkey_callbacks.clear()
            self.hotkey_id_to_name.clear()

        if hwnd:
            for info in registered_hotkeys:
                try:
                    windll.user32.UnregisterHotKey(hwnd, info['id'])
                except Exception:
                    pass

            try:
                if win32gui.IsWindow(hwnd):
                    win32gui.DestroyWindow(hwnd)
            except Exception as e:
                logging.debug(f"销毁热键窗口失败: {e}")

        try:
            win32gui.UnregisterClass(self._window_class_name, None)
        except Exception:
            pass

        self.hwnd = None
        self._thread_id = None
        self._listener_thread = None
        logging.info("热键监听线程已结束")

    def _is_window_handle_valid(self, hwnd=None) -> bool:
        """检查窗口句柄是否仍然有效。"""
        handle = self.hwnd if hwnd is None else hwnd
        if not handle:
            return False
        try:
            return bool(win32gui.IsWindow(handle))
        except Exception:
            return False

    def is_listener_alive(self) -> bool:
        """返回热键监听线程和窗口句柄是否都处于可用状态。"""
        thread = self._listener_thread
        return bool(thread and thread.is_alive() and self._is_window_handle_valid())

    def _request_stop(self, join_timeout: float = 0.0):
        """请求监听线程退出，可用于重建或关闭。"""
        self._running = False

        thread_id = self._thread_id
        if thread_id:
            try:
                win32api.PostThreadMessage(thread_id, win32con.WM_QUIT, 0, 0)
            except Exception as e:
                logging.debug(f"发送热键线程退出消息失败: {e}")

        thread = self._listener_thread
        if join_timeout and thread and thread.is_alive():
            thread.join(timeout=join_timeout)

    def update_ocr_hotkey(self, hotkey_str, callback):
        """更新 OCR 热键"""
        self._queue_update(hotkey_str, callback, "OCR")

    def update_translate_hotkey(self, hotkey_str, callback):
        """更新翻译热键"""
        self._queue_update(hotkey_str, callback, "翻译")

    def _queue_update(self, hotkey_str, callback, hotkey_name):
        """更新配置并请求线程刷新"""
        with self._lock:
            self.hotkey_configs[hotkey_name] = {'str': hotkey_str, 'callback': callback}
        self.re_register_hotkeys()

    def re_register_hotkeys(self):
        """
        触发重新注册热键。
        可以在任何线程安全地调用。
        """
        if not self.is_listener_alive():
            logging.warning("热键监听器不可用，尝试自动恢复")
            if not self.start_hotkey_listener():
                logging.error("热键监听器恢复失败，无法刷新热键")
                return False

        if self.hwnd:
            try:
                win32api.PostMessage(self.hwnd, WM_REFRESH_HOTKEYS, 0, 0)
                return True
            except Exception as e:
                logging.error(f"发送热键刷新消息失败: {e}")
        else:
            logging.warning("尝试注册热键，但窗口句柄尚未创建。")
        return False

    def _perform_registration_in_thread(self):
        """
        执行热键注册（只在热键线程中运行）。
        先注销所有已注册热键，再重新注册。
        """
        if not self._is_window_handle_valid():
            logging.warning("热键窗口句柄无效，跳过本次热键注册")
            return

        # 1. 注销所有已注册的热键
        with self._lock:
            for name, info in self.registered_hotkeys_by_name.items():
                try:
                    windll.user32.UnregisterHotKey(self.hwnd, info['id'])
                except Exception:
                    pass

            self.registered_hotkeys_by_name.clear()
            self.hotkey_callbacks.clear()
            self.hotkey_id_to_name.clear()  # 清空反向映射

            # 2. 重新注册所有配置的热键
            for name, config in self.hotkey_configs.items():
                hotkey_str = config['str']
                callback = config['callback']

                if not hotkey_str:
                    continue

                try:
                    modifiers, key = self._parse_hotkey(hotkey_str)

                    # 获取新的热键 ID（带溢出保护）
                    hotkey_id = self.next_hotkey_id
                    self.next_hotkey_id += 1
                    if self.next_hotkey_id > MAX_HOTKEY_ID:
                        self.next_hotkey_id = 1

                    if windll.user32.RegisterHotKey(self.hwnd, hotkey_id, modifiers, key):
                        self.hotkey_callbacks[hotkey_id] = callback
                        self.registered_hotkeys_by_name[name] = {'id': hotkey_id, 'str': hotkey_str}
                        self.hotkey_id_to_name[hotkey_id] = name  # 建立反向映射
                        logging.info(f"热键注册成功: {name} ({hotkey_str}) ID:{hotkey_id}")
                    else:
                        error_code = ctypes.get_last_error()
                        logging.error(f"注册 {name} 热键 '{hotkey_str}' 失败，错误码: {error_code}")

                except ValueError as e:
                    logging.error(f"解析 {name} 热键 '{hotkey_str}' 失败: {e}")
                except Exception as e:
                    logging.error(f"注册 {name} 热键 '{hotkey_str}' 发生异常: {e}")

    def _parse_hotkey(self, hotkey_str):
        """解析热键字符串为修饰符和虚拟键码"""
        modifiers = 0
        key = None
        parts = hotkey_str.upper().split('+')

        key_map = {
            'BACKSPACE': win32con.VK_BACK,
            'TAB': win32con.VK_TAB,
            'CLEAR': win32con.VK_CLEAR,
            'ENTER': win32con.VK_RETURN,
            'PAUSE': win32con.VK_PAUSE,
            'CAPSLOCK': win32con.VK_CAPITAL,
            'ESCAPE': win32con.VK_ESCAPE,
            'SPACE': win32con.VK_SPACE,
            'PAGEUP': win32con.VK_PRIOR,
            'PAGEDOWN': win32con.VK_NEXT,
            'END': win32con.VK_END,
            'HOME': win32con.VK_HOME,
            'LEFT': win32con.VK_LEFT,
            'UP': win32con.VK_UP,
            'RIGHT': win32con.VK_RIGHT,
            'DOWN': win32con.VK_DOWN,
            'SELECT': win32con.VK_SELECT,
            'PRINT': win32con.VK_PRINT,
            'EXECUTE': win32con.VK_EXECUTE,
            'PRINTSCREEN': win32con.VK_SNAPSHOT,
            'INSERT': win32con.VK_INSERT,
            'DELETE': win32con.VK_DELETE,
            'HELP': win32con.VK_HELP,
            'NUMPAD0': win32con.VK_NUMPAD0,
            'NUMPAD1': win32con.VK_NUMPAD1,
            'NUMPAD2': win32con.VK_NUMPAD2,
            'NUMPAD3': win32con.VK_NUMPAD3,
            'NUMPAD4': win32con.VK_NUMPAD4,
            'NUMPAD5': win32con.VK_NUMPAD5,
            'NUMPAD6': win32con.VK_NUMPAD6,
            'NUMPAD7': win32con.VK_NUMPAD7,
            'NUMPAD8': win32con.VK_NUMPAD8,
            'NUMPAD9': win32con.VK_NUMPAD9,
            'MULTIPLY': win32con.VK_MULTIPLY,
            'ADD': win32con.VK_ADD,
            'SEPARATOR': win32con.VK_SEPARATOR,
            'SUBTRACT': win32con.VK_SUBTRACT,
            'DECIMAL': win32con.VK_DECIMAL,
            'DIVIDE': win32con.VK_DIVIDE,
            'NUMLOCK': win32con.VK_NUMLOCK,
            'SCROLL': win32con.VK_SCROLL,
            'LSHIFT': win32con.VK_LSHIFT,
            'RSHIFT': win32con.VK_RSHIFT,
            'LCONTROL': win32con.VK_LCONTROL,
            'RCONTROL': win32con.VK_RCONTROL,
            'LALT': win32con.VK_LMENU,
            'RALT': win32con.VK_RMENU,
        }

        for part in parts:
            part = part.strip()
            if part == 'CTRL':
                modifiers |= win32con.MOD_CONTROL
            elif part == 'ALT':
                modifiers |= win32con.MOD_ALT
            elif part == 'SHIFT':
                modifiers |= win32con.MOD_SHIFT
            elif part == 'WIN' or part == 'META':
                modifiers |= win32con.MOD_WIN
            elif part.startswith('F') and len(part) <= 3:
                try:
                    key = getattr(win32con, f'VK_{part}')
                except AttributeError:
                    pass
            elif part in key_map:
                key = key_map[part]
            elif len(part) == 1:
                key = ord(part)

        if key is None:
            raise ValueError(f"无效的热键: {hotkey_str}")

        return modifiers, key

    def stop_listener(self):
        """停止热键监听"""
        self._request_stop(join_timeout=2.0)
        # 关闭回调线程池
        try:
            self._callback_executor.shutdown(wait=False)
            logging.debug("热键回调线程池已关闭")
        except Exception as e:
            logging.debug(f"关闭回调线程池失败: {e}")
        # 注意：不要在此处清除 _wndproc_wrapper 引用。
        # PostMessage(WM_QUIT) 是异步的，消息循环可能尚未退出，
        # 提前释放 WNDPROC 回调会导致悬挂指针崩溃。
        # 让 WNDPROC 引用随 HotkeyHandler 对象自然回收。

    def check_hotkey_conflict(self, hotkey_str: str) -> Dict:
        """
        Check if a hotkey can be registered (without actually registering it).

        Args:
            hotkey_str: The hotkey string to check (e.g., 'F4', 'Ctrl+Shift+O')

        Returns:
            Dict with keys:
                - available: bool - True if the hotkey can be registered
                - error_code: int - Windows error code if unavailable (0 if available)
                - error_msg: str - Human-readable error message
        """
        if not hotkey_str:
            return {
                'available': False,
                'error_code': ERROR_INVALID_PARAMETER,
                'error_msg': "热键不能为空"
            }

        try:
            modifiers, key = self._parse_hotkey(hotkey_str)
        except ValueError as e:
            return {
                'available': False,
                'error_code': ERROR_INVALID_PARAMETER,
                'error_msg': str(e)
            }

        # Try to register with a temporary ID
        temp_id = 0xFFFE  # Use a high ID unlikely to conflict
        result = windll.user32.RegisterHotKey(None, temp_id, modifiers, key)

        if result:
            # Success - unregister immediately
            windll.user32.UnregisterHotKey(None, temp_id)
            return {
                'available': True,
                'error_code': 0,
                'error_msg': ""
            }
        else:
            error_code = ctypes.get_last_error()
            error_msg = ERROR_MESSAGES.get(error_code, f"未知错误 (错误码: {error_code})")
            return {
                'available': False,
                'error_code': error_code,
                'error_msg': error_msg
            }

    def register_with_fallback(
        self,
        name: str,
        preferred_hotkey: str,
        callback: Callable,
        fallback_list: Optional[List[str]] = None
    ) -> Tuple[bool, str]:
        """
        Register a hotkey with automatic fallback if the preferred one is unavailable.

        Args:
            name: The hotkey name (e.g., 'OCR', '翻译')
            preferred_hotkey: The user's preferred hotkey
            callback: The callback function to execute
            fallback_list: Optional list of fallback hotkeys to try

        Returns:
            Tuple of (success: bool, registered_hotkey: str)
            - If successful, returns (True, actual_hotkey_registered)
            - If all fallbacks fail, returns (False, "")
        """
        # Build the list of hotkeys to try
        hotkeys_to_try = [preferred_hotkey]

        # Add fallbacks from the provided list or default fallbacks
        if fallback_list:
            for hk in fallback_list:
                if hk not in hotkeys_to_try:
                    hotkeys_to_try.append(hk)
        elif name in FALLBACK_HOTKEYS:
            for hk in FALLBACK_HOTKEYS[name]:
                if hk not in hotkeys_to_try:
                    hotkeys_to_try.append(hk)

        # Try each hotkey
        for i, hotkey in enumerate(hotkeys_to_try):
            check_result = self.check_hotkey_conflict(hotkey)

            if check_result['available']:
                # Queue the update
                self._queue_update(hotkey, callback, name)

                if i > 0:
                    logging.warning(
                        f"热键 '{preferred_hotkey}' 不可用，已使用备选热键 '{hotkey}'"
                    )
                return (True, hotkey)
            else:
                logging.debug(
                    f"热键 '{hotkey}' 不可用: {check_result['error_msg']}"
                )

        # All hotkeys failed
        logging.error(f"所有热键备选项均不可用: {hotkeys_to_try}")
        return (False, "")

    def get_fallback_hotkeys(self, name: str) -> List[str]:
        """
        Get the predefined fallback hotkeys for a given hotkey name.

        Args:
            name: The hotkey name (e.g., 'OCR', '翻译')

        Returns:
            List of fallback hotkey strings
        """
        return FALLBACK_HOTKEYS.get(name, [])

    def is_hotkey_registered(self, name: str) -> bool:
        """
        Check if a hotkey with the given name is currently registered.

        Args:
            name: The hotkey name

        Returns:
            True if the hotkey is registered
        """
        with self._lock:
            return name in self.registered_hotkeys_by_name

    def get_registered_hotkey(self, name: str) -> Optional[str]:
        """
        Get the currently registered hotkey string for a given name.

        Args:
            name: The hotkey name

        Returns:
            The hotkey string if registered, None otherwise
        """
        with self._lock:
            info = self.registered_hotkeys_by_name.get(name)
            return info['str'] if info else None
