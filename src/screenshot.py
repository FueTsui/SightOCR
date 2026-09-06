# screenshot.py

import tkinter as tk
import sys
import mss
import mss.tools
from datetime import datetime
import os
from pathlib import Path
import tempfile
import logging
import threading
import time
from PIL import Image, ImageTk
from .utils import resource_path

SCREENSHOT_DIR = os.path.join(tempfile.gettempdir(), "SightOCR", "screenshots")
CLEANUP_INTERVAL = 86400  # 清理间隔（秒）
FILE_MAX_AGE = 72 * 3600  # 文件最大保留时间（秒）
_CURSOR_IMAGE_CACHE = None
_CURSOR_IMAGE_LOCK = threading.Lock()
_DARKEN_LUT = [value // 2 for value in range(256)] * 3
_SELECTOR_INSTANCE = None
_SELECTOR_THREAD_ID = None
_SELECTOR_LOCK = threading.Lock()


def _log_perf(trace_label, message, level=logging.INFO):
    """Write a screenshot performance log if tracing is enabled."""
    if trace_label:
        logging.log(level, f"[perf:{trace_label}] {message}")


def get_cursor_path():
    """获取光标图片路径"""
    cursor_path = resource_path(os.path.join("assets", "cursor", "cross.png"))
    logging.debug(f"Cursor image path: {cursor_path}")
    return cursor_path


def get_cached_cursor_image():
    """Load and cache the custom cursor image once per process."""
    global _CURSOR_IMAGE_CACHE

    if _CURSOR_IMAGE_CACHE is not None:
        return _CURSOR_IMAGE_CACHE

    with _CURSOR_IMAGE_LOCK:
        if _CURSOR_IMAGE_CACHE is not None:
            return _CURSOR_IMAGE_CACHE

        cursor_path = get_cursor_path()
        try:
            _CURSOR_IMAGE_CACHE = Image.open(cursor_path).convert("RGBA")
            logging.debug("自定义光标图片已缓存")
        except FileNotFoundError:
            logging.debug("自定义光标图片不存在，使用默认光标")
            _CURSOR_IMAGE_CACHE = None
        except Exception as e:
            logging.warning(f"加载自定义光标失败: {e}")
            _CURSOR_IMAGE_CACHE = None

    return _CURSOR_IMAGE_CACHE


class _ReusableScreenshotSelector:
    """Persistent Tk screenshot selector reused across captures on the same thread."""

    def __init__(self):
        self.root = tk.Tk()
        self.root.withdraw()
        self.root.overrideredirect(True)
        self.root.attributes("-topmost", True)

        self.canvas = tk.Canvas(self.root, highlightthickness=0, cursor="crosshair")
        self.canvas.pack(fill="both", expand=True)

        self.bg_tk_img = None
        self.bg_image_obj = self.canvas.create_image(0, 0, anchor="nw")
        self.selection_image_obj = self.canvas.create_image(0, 0, anchor="nw")
        self.selection_border = self.canvas.create_rectangle(0, 0, 0, 0, outline="", width=0)
        self.selection_tk_img = None

        self.cursor_img = get_cached_cursor_image()
        self.cursor_offset = (0, 0)
        self.cursor_tk_img = None
        self.cursor_obj = None
        if self.cursor_img is not None:
            self.cursor_offset = (self.cursor_img.width // 2, self.cursor_img.height // 2)
            self.cursor_tk_img = ImageTk.PhotoImage(self.cursor_img)
            self.cursor_obj = self.canvas.create_image(-100, -100, anchor="nw", image=self.cursor_tk_img)
            self.canvas.configure(cursor="none")

        self.trace_label = None
        self.pil_img = None
        self.screen_left = 0
        self.screen_top = 0
        self.screen_width = 0
        self.screen_height = 0
        self._throttle_ms = 8
        self.state = {}

        self.canvas.bind("<ButtonPress-1>", self._on_mouse_down)
        self.canvas.bind("<B1-Motion>", self._on_mouse_drag)
        self.canvas.bind("<ButtonRelease-1>", self._on_mouse_up)
        self.canvas.bind("<Motion>", self._on_mouse_move)
        self.root.bind("<Escape>", self._on_escape)

    def is_usable(self) -> bool:
        """Return whether the underlying Tk objects are still valid."""
        try:
            return bool(self.root.winfo_exists())
        except tk.TclError:
            return False

    def begin_selection(self, pil_img, dark_img, monitor, trace_label=None, started_at=None):
        """Prepare the selector UI and run the modal selection loop."""
        self.trace_label = trace_label
        self.pil_img = pil_img
        self.screen_left = monitor["left"]
        self.screen_top = monitor["top"]
        self.screen_width = monitor["width"]
        self.screen_height = monitor["height"]
        self._reset_state()

        self.root.geometry(
            f"{self.screen_width}x{self.screen_height}+{self.screen_left}+{self.screen_top}"
        )
        self.canvas.configure(width=self.screen_width, height=self.screen_height)

        if (
            self.bg_tk_img is None
            or self.bg_tk_img.width() != dark_img.width
            or self.bg_tk_img.height() != dark_img.height
        ):
            self.bg_tk_img = ImageTk.PhotoImage(dark_img)
            self.canvas.itemconfig(self.bg_image_obj, image=self.bg_tk_img)
        else:
            self.bg_tk_img.paste(dark_img)
        self.canvas.coords(self.bg_image_obj, 0, 0)

        self.root.update_idletasks()
        self.root.deiconify()
        self.root.focus_force()
        self.root.update()
        self._show_cursor_at_pointer()
        if started_at is not None:
            ready_elapsed_ms = (time.perf_counter() - started_at) * 1000
            _log_perf(self.trace_label, f"Screenshot selector window ready; elapsed={ready_elapsed_ms:.1f}ms")

        loop_started_at = time.perf_counter()
        self.root.mainloop()
        loop_elapsed_ms = (time.perf_counter() - loop_started_at) * 1000
        _log_perf(self.trace_label, f"Screenshot selector window closed; interaction={loop_elapsed_ms:.1f}ms")
        self.root.withdraw()
        self._release_capture_resources()

        if self.state['cancelled']:
            return None

        left = min(self.state['start_x'], self.state['end_x'])
        top = min(self.state['start_y'], self.state['end_y'])
        right = max(self.state['start_x'], self.state['end_x'])
        bottom = max(self.state['start_y'], self.state['end_y'])

        if right > left and bottom > top:
            return (left, top, right, bottom)
        return None

    def _release_capture_resources(self):
        """选区结束后释放本次截图持有的全屏位图。

        选择器实例为常驻复用对象，若不主动释放，全屏 PIL 图与暗化背景的
        Tk PhotoImage（各为 屏宽x屏高 的 24/32bpp 位图）会在后台驻留期间
        一直占用数十 MB 内存。下次截图时重新创建 PhotoImage 与 paste 复用
        同样需要整帧拷贝，成本相当（毫秒级），不影响截图响应速度。
        """
        self.pil_img = None
        self.selection_tk_img = None
        self.canvas.itemconfig(self.selection_image_obj, image="")
        if self.bg_tk_img is not None:
            self.canvas.itemconfig(self.bg_image_obj, image="")
            self.bg_tk_img = None

    def _reset_state(self):
        """Reset selector state for a new capture session."""
        self.state = {
            'start_x': 0,
            'start_y': 0,
            'end_x': 0,
            'end_y': 0,
            'cancelled': False,
            'last_update_time': 0,
            'pending_update': None,
            'mouse_down_at': None,
            'mouse_up_at': None,
            'visual_updates': 0,
            'visual_update_total_ms': 0.0,
            'visual_update_max_ms': 0.0,
        }
        self.canvas.itemconfig(self.selection_border, outline="", width=0)
        self.canvas.coords(self.selection_border, 0, 0, 0, 0)
        self.selection_tk_img = None
        self.canvas.itemconfig(self.selection_image_obj, image="")
        self.canvas.coords(self.selection_image_obj, -100, -100)
        if self.cursor_obj:
            self.canvas.coords(self.cursor_obj, -100, -100)

    def _update_cursor(self, event):
        """Update the custom cursor position."""
        self._set_cursor_screen_position(event.x_root, event.y_root)

    def _set_cursor_screen_position(self, screen_x, screen_y):
        """Place the custom cursor using absolute screen coordinates."""
        if self.cursor_obj:
            x = screen_x - self.screen_left - self.cursor_offset[0]
            y = screen_y - self.screen_top - self.cursor_offset[1]
            self.canvas.coords(self.cursor_obj, x, y)
            self.canvas.tag_raise(self.cursor_obj)

    def _show_cursor_at_pointer(self):
        """Render the custom cursor immediately when the selector opens."""
        if not self.cursor_obj:
            return

        self._set_cursor_screen_position(
            self.root.winfo_pointerx(),
            self.root.winfo_pointery(),
        )

    def _do_visual_update(self, x1, y1, x2, y2):
        """Refresh the highlighted selection area."""
        update_started_at = time.perf_counter()
        if x2 > x1 and y2 > y1:
            cropped_pil = self.pil_img.crop(
                (
                    x1 - self.screen_left,
                    y1 - self.screen_top,
                    x2 - self.screen_left,
                    y2 - self.screen_top,
                )
            )
            self.selection_tk_img = ImageTk.PhotoImage(cropped_pil)
            self.canvas.itemconfig(self.selection_image_obj, image=self.selection_tk_img)
            self.canvas.coords(
                self.selection_image_obj,
                x1 - self.screen_left,
                y1 - self.screen_top,
            )
            self.canvas.itemconfig(self.selection_border, outline="#FFFFFF", width=1)
            self.canvas.coords(
                self.selection_border,
                x1 - self.screen_left,
                y1 - self.screen_top,
                x2 - self.screen_left,
                y2 - self.screen_top
            )
            self.canvas.tag_raise(self.selection_border)
            if self.cursor_obj:
                self.canvas.tag_raise(self.cursor_obj)
            update_elapsed_ms = (time.perf_counter() - update_started_at) * 1000
            self.state['visual_updates'] += 1
            self.state['visual_update_total_ms'] += update_elapsed_ms
            self.state['visual_update_max_ms'] = max(self.state['visual_update_max_ms'], update_elapsed_ms)

    def _on_mouse_down(self, event):
        self.state['mouse_down_at'] = time.perf_counter()
        self.state['start_x'], self.state['start_y'] = event.x_root, event.y_root
        _log_perf(
            self.trace_label,
            f"Selection drag started; x={event.x_root} y={event.y_root}"
        )
        self._update_cursor(event)

    def _on_mouse_drag(self, event):
        self.state['end_x'], self.state['end_y'] = event.x_root, event.y_root
        x1 = min(self.state['start_x'], self.state['end_x'])
        y1 = min(self.state['start_y'], self.state['end_y'])
        x2 = max(self.state['start_x'], self.state['end_x'])
        y2 = max(self.state['start_y'], self.state['end_y'])

        current_time = time.time() * 1000
        if current_time - self.state['last_update_time'] >= self._throttle_ms:
            self.state['last_update_time'] = current_time
            self._do_visual_update(x1, y1, x2, y2)
        else:
            self.state['pending_update'] = (x1, y1, x2, y2)

        self._update_cursor(event)

    def _on_mouse_up(self, event):
        self.state['mouse_up_at'] = time.perf_counter()
        self.state['end_x'], self.state['end_y'] = event.x_root, event.y_root
        if self.state['pending_update']:
            self._do_visual_update(*self.state['pending_update'])
            self.state['pending_update'] = None

        width = abs(self.state['end_x'] - self.state['start_x'])
        height = abs(self.state['end_y'] - self.state['start_y'])
        drag_elapsed_ms = 0.0
        if self.state['mouse_down_at'] is not None:
            drag_elapsed_ms = (self.state['mouse_up_at'] - self.state['mouse_down_at']) * 1000
        avg_visual_update_ms = 0.0
        if self.state['visual_updates'] > 0:
            avg_visual_update_ms = self.state['visual_update_total_ms'] / self.state['visual_updates']
        _log_perf(
            self.trace_label,
            f"Selection drag finished; duration={drag_elapsed_ms:.1f}ms size={width}x{height} "
            f"updates={self.state['visual_updates']} avg_update={avg_visual_update_ms:.3f}ms "
            f"max_update={self.state['visual_update_max_ms']:.3f}ms"
        )
        self.root.quit()

    def _on_escape(self, event):
        self.state['cancelled'] = True
        _log_perf(self.trace_label, "Screenshot selection cancelled by Escape", level=logging.WARNING)
        self.root.quit()

    def _on_mouse_move(self, event):
        self._update_cursor(event)


def get_reusable_selector(trace_label=None):
    """Return a persistent selector instance for the current thread."""
    global _SELECTOR_INSTANCE, _SELECTOR_THREAD_ID

    thread_id = threading.get_ident()
    with _SELECTOR_LOCK:
        needs_new_selector = (
            _SELECTOR_INSTANCE is None
            or _SELECTOR_THREAD_ID != thread_id
            or not _SELECTOR_INSTANCE.is_usable()
        )
        if needs_new_selector:
            _SELECTOR_INSTANCE = _ReusableScreenshotSelector()
            _SELECTOR_THREAD_ID = thread_id
            _log_perf(trace_label, "Screenshot selector instance created")
        else:
            _log_perf(trace_label, "Screenshot selector instance reused")
        return _SELECTOR_INSTANCE


def prewarm_screenshot_selector(trace_label=None):
    """预热截图选择器，消除开机后首次热键截图的冷启动延迟。

    首次按热键截图时，Tk 根窗口、ImageTk 原生模块、mss 屏幕抓取均为冷启动，
    在开机后磁盘缓存未命中的情况下合计可达数秒。本函数在程序启动后的空闲时段
    预先支付这些一次性成本，使首次截图与后续截图一样无延迟。

    重要：必须在“稍后实际执行 take_screenshot 的同一线程”（OCR 工作线程）上调用，
    因为 Tk 对象具有线程亲和性——选择器实例按线程 ID 缓存复用。
    """
    started_at = time.perf_counter()
    try:
        # 1. 预热自定义光标图片缓存（进程级单例）
        get_cached_cursor_image()

        # 2. 在当前线程创建持久化 Tk 选择器（支付 Tk/tcl 初始化与 DLL 加载成本，
        #    其 __init__ 内创建 ImageTk.PhotoImage，同时预热 ImageTk 原生模块）
        get_reusable_selector(trace_label=trace_label)

        # 3. 预热 mss：执行一次极小区域抓取，加载 mss 原生库并初始化屏幕 DC，
        #    避免首次截图时才支付该成本（不抓取屏幕内容，仅 1x1 像素）
        try:
            with mss.mss() as sct:
                sct.grab({"left": 0, "top": 0, "width": 1, "height": 1})
        except Exception as e:
            logging.debug(f"mss 预热失败（不影响功能）: {e}")

        elapsed_ms = (time.perf_counter() - started_at) * 1000
        logging.info(f"截图选择器预热完成，耗时 {elapsed_ms:.1f}ms")
        _log_perf(trace_label, f"Screenshot selector prewarmed; duration={elapsed_ms:.1f}ms")
        return True
    except Exception as e:
        elapsed_ms = (time.perf_counter() - started_at) * 1000
        logging.warning(f"截图选择器预热失败（首次热键截图可能仍有延迟），耗时 {elapsed_ms:.1f}ms: {e}")
        return False


def ensure_screenshot_dir():
    """确保截图目录存在，失败时返回 False"""
    try:
        Path(SCREENSHOT_DIR).mkdir(parents=True, exist_ok=True)
        return True
    except PermissionError as e:
        logging.error(f"创建截图目录失败（权限错误）: {e}")
        return False
    except Exception as e:
        logging.error(f"创建截图目录失败: {e}")
        return False


def select_screenshot_area(trace_label=None):
    """选择截图区域，返回 (bbox, pil_img) 或 (None, None)"""
    pil_img = None
    started_at = time.perf_counter()

    try:
        _log_perf(trace_label, "Screenshot selection started")

        # 截取全屏
        capture_started_at = time.perf_counter()
        with mss.mss() as sct:
            monitor = sct.monitors[0]
            sct_img = sct.grab(monitor)
            pil_img = Image.frombytes("RGB", sct_img.size, sct_img.bgra, "raw", "BGRX")
        capture_elapsed_ms = (time.perf_counter() - capture_started_at) * 1000
        _log_perf(
            trace_label,
            f"Fullscreen snapshot captured; duration={capture_elapsed_ms:.1f}ms "
            f"size={pil_img.size[0]}x{pil_img.size[1]}"
        )

        screen_left = monitor["left"]
        screen_top = monitor["top"]
        screen_width = monitor["width"]
        screen_height = monitor["height"]

        # 创建暗化的背景图（静态截图 + 半透明遮罩效果）
        overlay_started_at = time.perf_counter()
        dark_img = pil_img.point(_DARKEN_LUT)
        overlay_elapsed_ms = (time.perf_counter() - overlay_started_at) * 1000
        _log_perf(trace_label, f"Screenshot overlay prepared; duration={overlay_elapsed_ms:.1f}ms")

        selector = get_reusable_selector(trace_label=trace_label)
        selection_started_at = time.perf_counter()
        selected_bounds = selector.begin_selection(
            pil_img,
            dark_img,
            monitor,
            trace_label=trace_label,
            started_at=started_at
        )
        if selected_bounds is None:
            total_elapsed_ms = (time.perf_counter() - started_at) * 1000
            _log_perf(trace_label, f"Screenshot selection ended without area; total={total_elapsed_ms:.1f}ms")
            return None, None

        left, top, right, bottom = selected_bounds
        crop_box = (left - screen_left, top - screen_top, right - screen_left, bottom - screen_top)
        selection_elapsed_ms = (time.perf_counter() - selection_started_at) * 1000
        total_elapsed_ms = (time.perf_counter() - started_at) * 1000
        _log_perf(
            trace_label,
            f"Screenshot area selected; selection={selection_elapsed_ms:.1f}ms "
            f"total={total_elapsed_ms:.1f}ms bbox=({left},{top},{right},{bottom})"
        )
        return crop_box, pil_img

    except Exception as e:
        total_elapsed_ms = (time.perf_counter() - started_at) * 1000
        _log_perf(
            trace_label,
            f"Screenshot selection failed after {total_elapsed_ms:.1f}ms: {e}",
            level=logging.ERROR
        )
        logging.error(f"截图选区失败: {e}", exc_info=True)
        return None, None


def take_screenshot(trace_label=None):
    """截取屏幕区域并保存为文件"""
    started_at = time.perf_counter()
    try:
        crop_box, pil_img = select_screenshot_area(trace_label=trace_label)
        if crop_box and pil_img:
            # 从预截取的静态图像中裁剪选区
            crop_started_at = time.perf_counter()
            cropped = pil_img.crop(crop_box)
            crop_elapsed_ms = (time.perf_counter() - crop_started_at) * 1000
            _log_perf(trace_label, f"Screenshot cropped from cached image; duration={crop_elapsed_ms:.1f}ms")

            if not ensure_screenshot_dir():
                # 如果无法创建目录，尝试使用系统临时目录
                filepath = os.path.join(tempfile.gettempdir(), f"sight_{datetime.now().strftime('%Y%m%d_%H%M%S')}.png")
            else:
                filepath = os.path.join(SCREENSHOT_DIR, f"sight_{datetime.now().strftime('%Y%m%d_%H%M%S')}.png")

            save_started_at = time.perf_counter()
            cropped.save(filepath, "PNG")
            save_elapsed_ms = (time.perf_counter() - save_started_at) * 1000
            total_elapsed_ms = (time.perf_counter() - started_at) * 1000
            _log_perf(
                trace_label,
                f"Screenshot saved; save_duration={save_elapsed_ms:.1f}ms total={total_elapsed_ms:.1f}ms "
                f"path={filepath}"
            )
            logging.debug(f"截图已保存: {filepath}")
            return filepath
        total_elapsed_ms = (time.perf_counter() - started_at) * 1000
        _log_perf(trace_label, f"take_screenshot finished without image; total={total_elapsed_ms:.1f}ms")
        return None
    except Exception as e:
        total_elapsed_ms = (time.perf_counter() - started_at) * 1000
        _log_perf(trace_label, f"take_screenshot failed after {total_elapsed_ms:.1f}ms: {e}", level=logging.ERROR)
        logging.error(f"截图保存失败: {e}", exc_info=True)
        return None


def cleanup_old_files():
    """清理过期的截图文件"""
    if not os.path.exists(SCREENSHOT_DIR):
        return

    current_time = time.time()
    cleaned_count = 0

    try:
        for filename in os.listdir(SCREENSHOT_DIR):
            file_path = os.path.join(SCREENSHOT_DIR, filename)
            if os.path.isfile(file_path):
                try:
                    file_age = current_time - os.path.getmtime(file_path)
                    if file_age > FILE_MAX_AGE:
                        os.remove(file_path)
                        cleaned_count += 1
                except PermissionError:
                    logging.debug(f"无法删除文件（权限错误）: {file_path}")
                except FileNotFoundError:
                    pass  # 文件已被删除
                except Exception as e:
                    logging.debug(f"清理文件失败: {file_path}, 错误: {e}")

        if cleaned_count > 0:
            logging.debug(f"已清理 {cleaned_count} 个过期截图文件")
    except Exception as e:
        logging.debug(f"清理截图目录失败: {e}")


# 清理线程控制标志
_cleanup_thread_running = False
_cleanup_thread_stop_event = threading.Event()


def start_cleanup_thread():
    """启动后台清理线程"""
    global _cleanup_thread_running

    if _cleanup_thread_running:
        logging.debug("截图清理线程已在运行")
        return

    _cleanup_thread_stop_event.clear()
    _cleanup_thread_running = True

    def run_cleanup():
        global _cleanup_thread_running
        while not _cleanup_thread_stop_event.is_set():
            try:
                cleanup_old_files()
            except Exception as e:
                logging.debug(f"清理线程异常: {e}")
            # 使用 wait 代替 sleep，支持优雅退出
            _cleanup_thread_stop_event.wait(timeout=CLEANUP_INTERVAL)
        _cleanup_thread_running = False
        logging.debug("截图清理线程已停止")

    t = threading.Thread(target=run_cleanup, daemon=True, name="ScreenshotCleanup")
    t.start()
    logging.debug("截图清理线程已启动")


def stop_cleanup_thread():
    """停止后台清理线程（优雅退出）"""
    global _cleanup_thread_running
    if _cleanup_thread_running:
        _cleanup_thread_stop_event.set()
        logging.debug("已发送清理线程停止信号")
