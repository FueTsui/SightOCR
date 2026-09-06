# ocr_manager.py
"""
OCRManager for managing OCR requests with fallback chain.
Extracted from MainWindow to reduce its size.
"""

import logging
import queue
import threading
import time
from typing import Callable, Optional, Tuple

from PySide6.QtCore import QObject, Signal

from ..ocr import use_oneocr, use_oneocr_table, use_baidu_ocr, use_tencent_ocr, init_oneocr, _ensure_table_deps
from ..screenshot import take_screenshot, prewarm_screenshot_selector
from ..async_worker import prewarm_translation_tokens
from .config_manager import ConfigManager

# 队列标记：请求在工作线程上预热截图选择器（Tk 对象具有线程亲和性，
# 必须在实际执行截图的同一线程上创建）
_PREWARM_SELECTOR_MARKER = "__PREWARM_SELECTOR__"


class OCRManager(QObject):
    """
    Manages OCR requests with queue-based processing and fallback chain.

    Usage:
        ocr_mgr = OCRManager()
        ocr_mgr.ocr_completed.connect(on_ocr_done)
        ocr_mgr.ocr_error.connect(on_ocr_error)
        ocr_mgr.start()
        ocr_mgr.request_ocr(auto_translate=True, show_window=True)
    """

    # Emitted when OCR completes successfully (text, auto_translate, show_window)
    ocr_completed = Signal(str, bool, bool)

    # Emitted when an error occurs (title, message)
    ocr_error = Signal(str, str)

    # Emitted when OCR selection should be updated in UI
    ocr_selection_changed = Signal(str)

    def __init__(self, parent=None):
        super().__init__(parent)

        self._queue = queue.Queue()
        self._worker_thread = None
        self._running = False
        self._in_progress = False
        self._lock = threading.Lock()
        self._preinit_lock = threading.Lock()
        self._oneocr_preinit_started = False
        self._table_deps_preload_started = False

        self._config = ConfigManager.instance()

    def start(self):
        """Start the OCR worker thread."""
        if self._running:
            return

        self._running = True
        self._worker_thread = threading.Thread(
            target=self._process_queue,
            daemon=True,
            name="OCRWorker"
        )
        self._worker_thread.start()
        logging.debug("OCRManager worker thread started")

    def stop(self):
        """Stop the OCR worker thread."""
        self._running = False
        # Add sentinel to unblock the queue
        self._queue.put(None)

    def request_ocr(
        self,
        auto_translate: bool = False,
        show_window: bool = True,
        trace_label: Optional[str] = None
    ) -> bool:
        """
        Request an OCR operation.

        Args:
            auto_translate: Whether to auto-translate after OCR
            show_window: Whether to show the main window after OCR

        Returns:
            True if request was queued, False if OCR is already in progress
        """
        with self._lock:
            if self._in_progress:
                logging.warning("OCR already in progress, ignoring new request")
                return False
            self._in_progress = True
            logging.debug("OCR request queued")

        queued_at = time.perf_counter()
        if trace_label:
            logging.info(f"[perf:{trace_label}] OCR request queued")

        self._queue.put((auto_translate, show_window, trace_label, queued_at))
        return True

    def is_in_progress(self) -> bool:
        """Return whether an OCR operation is in progress."""
        with self._lock:
            return self._in_progress

    def request_selector_prewarm(self) -> None:
        """请求在 OCR 工作线程上预热截图选择器。

        消除开机自启后首次热键截图约 3 秒的冷启动延迟。预热任务在工作线程上执行，
        与实际截图共用同一线程，从而满足 Tk 的线程亲和性要求。
        """
        self._queue.put(_PREWARM_SELECTOR_MARKER)

    def pre_init_default_ocr(self):
        """Pre-initialize the default OCR engine in background."""
        self.preload_for_selection(self._config.last_ocr_selection)

    def preload_for_selection(self, selection: str):
        """按需预热所选 OCR 接口的重量级资源（线程安全，幂等）。

        - 默认 / 默认_table：预初始化 OneOCR 引擎
        - 默认_table：额外预加载本地表格识别依赖

        本地表格识别依赖（numpy/scipy/cv2/sklearn）常驻约 110MB 内存，
        仅在“默认_table”被选中时才预加载；其余模式（含云端表格/公式接口）
        完全不需要这些库，延迟到首次本地表格识别时再加载，
        避免后台空闲时白白占用上百 MB。
        """
        should_init_oneocr = False
        should_preload_table_deps = False

        with self._preinit_lock:
            if (
                selection in ('默认', '默认_table')
                and not self._oneocr_preinit_started
            ):
                self._oneocr_preinit_started = True
                should_init_oneocr = True

            if selection == '默认_table' and not self._table_deps_preload_started:
                self._table_deps_preload_started = True
                should_preload_table_deps = True

        if should_init_oneocr:
            threading.Thread(
                target=self._init_oneocr_safe,
                daemon=True,
                name="OCRPreInit"
            ).start()

        if should_preload_table_deps:
            threading.Thread(
                target=self._preload_table_deps,
                daemon=True,
                name="TableDepsPreload"
            ).start()

    def _init_oneocr_safe(self):
        """Safely initialize OneOCR."""
        try:
            logging.info("Pre-initializing default OCR (OneOCR)...")
            init_oneocr()
            logging.info("Default OCR (OneOCR) pre-initialized successfully")
        except Exception as e:
            logging.error(f"Failed to pre-initialize default OCR: {e}")
            self.ocr_error.emit("OCR 初始化失败", f"无法初始化默认 OCR: {e}")

    def _preload_table_deps(self):
        """预加载表格识别依赖库（numpy/scipy/cv2/sklearn）。"""
        try:
            logging.info("预加载表格识别依赖库...")
            _ensure_table_deps()
            logging.info("表格识别依赖库预加载完成")
        except Exception as e:
            logging.error(f"预加载表格识别依赖库失败: {e}")

    def _process_queue(self):
        """Process OCR requests from the queue."""
        while self._running:
            item = None
            try:
                item = self._queue.get()
                if item is None:  # Sentinel
                    break

                # 预热标记：在工作线程上创建并预热截图选择器后继续等待，
                # 不涉及 _in_progress 状态
                if item == _PREWARM_SELECTOR_MARKER:
                    prewarm_screenshot_selector()
                    self._queue.task_done()
                    continue

                auto_translate, show_window, trace_label, queued_at = item
                self._execute_ocr(auto_translate, show_window, trace_label, queued_at)
                self._queue.task_done()

            except Exception as e:
                logging.error(f"Error in OCR worker thread: {e}", exc_info=True)
            finally:
                # 只有在确实处理了 OCR 请求时才重置状态（预热标记已在上方 continue）
                if item is not None and item != _PREWARM_SELECTOR_MARKER:
                    with self._lock:
                        self._in_progress = False
                        logging.debug("OCR request completed")

    def _execute_ocr(
        self,
        auto_translate: bool,
        show_window: bool,
        trace_label: Optional[str] = None,
        queued_at: Optional[float] = None
    ):
        """Execute the OCR operation with fallback chain."""
        try:
            ocr_started_at = time.perf_counter()
            if trace_label:
                queue_wait_ms = 0.0 if queued_at is None else (ocr_started_at - queued_at) * 1000
                logging.info(
                    f"[perf:{trace_label}] OCR execution started; queue_wait={queue_wait_ms:.1f}ms"
                )

            # If auto_translate is enabled, start prewarming translation tokens in parallel
            prewarm_thread = None
            if auto_translate and self._config.last_translate_selection == '默认':
                prewarm_thread = threading.Thread(
                    target=prewarm_translation_tokens,
                    daemon=True,
                    name="TranslationPrewarm"
                )
                prewarm_thread.start()
                logging.debug("Started translation token prewarming in parallel with OCR")

            # Take screenshot
            image_path = take_screenshot(trace_label=trace_label)
            if not image_path:
                self.ocr_error.emit("提示", "未选择有效的截屏区域。")
                return
            if trace_label:
                logging.info(f"[perf:{trace_label}] Screenshot file ready; path={image_path}")

            user_selection = self._config.last_ocr_selection

            # Handle F2 with special OCR modes
            special_modes = ['默认_table', 'Baidu_table', 'Baidu_formula', 'Tencent_table', 'Tencent_formula']
            if auto_translate and user_selection in special_modes:
                if user_selection == "默认_table":
                    new_selection = "默认"
                elif user_selection.startswith("Baidu_"):
                    new_selection = "Baidu_auto"
                elif user_selection.startswith("Tencent_"):
                    new_selection = "Tencent_auto"
                else:
                    new_selection = "默认"
                logging.info(f"F2 Translate used with '{user_selection}'. Switching to '{new_selection}'.")
                self._config.last_ocr_selection = new_selection
                self._config.save()
                self.ocr_selection_changed.emit(new_selection)
                user_selection = new_selection

            # Execute OCR with fallback chain
            ocr_text = self._execute_with_fallback(image_path, user_selection)

            # 格式选项：将识别文本的换行符替换为空格
            # 表格/公式模式依赖换行排版，不做处理
            if (
                ocr_text
                and self._config.replace_newline
                and user_selection not in special_modes
            ):
                ocr_text = (
                    ocr_text.replace('\r\n', '\n')
                    .replace('\r', '\n')
                    .replace('\n', ' ')
                )

            # Handle result
            if ocr_text is not None:
                if trace_label:
                    ocr_duration_ms = (time.perf_counter() - ocr_started_at) * 1000
                    logging.info(
                        f"[perf:{trace_label}] OCR completed; duration={ocr_duration_ms:.1f}ms "
                        f"chars={len(ocr_text)}"
                    )
                self.ocr_completed.emit(ocr_text, auto_translate, show_window)
            else:
                if trace_label:
                    ocr_duration_ms = (time.perf_counter() - ocr_started_at) * 1000
                    logging.warning(
                        f"[perf:{trace_label}] OCR completed with empty result; "
                        f"duration={ocr_duration_ms:.1f}ms"
                    )
                if image_path:
                    self.ocr_error.emit("OCR 提示", "所有OCR接口均未能识别到内容，请重试。")

        except Exception as e:
            if trace_label:
                ocr_duration_ms = (time.perf_counter() - ocr_started_at) * 1000
                logging.error(
                    f"[perf:{trace_label}] OCR execution failed after {ocr_duration_ms:.1f}ms: {e}"
                )
            logging.error(f"OCR error: {e}", exc_info=True)
            self.ocr_error.emit("OCR 提示", f"OCR 识别过程中发生错误：{e}")

    def _execute_with_fallback(self, image_path: str, user_selection: str) -> Optional[str]:
        """
        Execute OCR with dynamic fallback chain.

        Args:
            image_path: Path to the image file
            user_selection: User's selected OCR source

        Returns:
            OCR result text or None
        """
        # Map old Tencent selections
        if user_selection in ["Tencent_general_basic", "Tencent_general_accurate"]:
            user_selection = "Tencent_auto"

        # Build API chain based on selection
        apis_to_try = self._build_api_chain(user_selection)

        # Execute chain
        for i, api_id in enumerate(apis_to_try):
            if i > 0:
                self.ocr_error.emit("OCR 提示", "接口调用失败，自动尝试下一接口...")
                logging.warning(f"Fallback to OCR API: {api_id}")

            result = self._try_api(api_id, image_path, user_selection, i == 0)
            if result is not None:
                return result

        return None

    def _build_api_chain(self, user_selection: str) -> list:
        """Build the API fallback chain based on user selection."""
        general_text_chain = [
            'Baidu_accurate_basic', 'Baidu_accurate',
            'Baidu_general_basic', 'Baidu_general'
        ]

        if user_selection == '默认_table':
            return ['默认_table']
        elif user_selection == 'Baidu_auto':
            return general_text_chain + ['默认']
        elif user_selection in general_text_chain:
            start_index = general_text_chain.index(user_selection)
            return general_text_chain[start_index:] + ['默认']
        elif user_selection in ['Baidu_table', 'Baidu_formula']:
            return [user_selection, '默认']
        elif user_selection == 'Tencent_auto':
            return ['Tencent_general_accurate', 'Tencent_general_basic', '默认']
        elif user_selection in ['Tencent_table', 'Tencent_formula']:
            return [user_selection, '默认']
        else:
            return ['默认']

    def _try_api(self, api_id: str, image_path: str,
                 user_selection: str, is_first: bool) -> Optional[str]:
        """
        Try a specific OCR API.

        Returns:
            OCR text result, or None to continue to next API
        """
        if api_id == "默认_table":
            return self._try_default_table_api(image_path)
        elif api_id.startswith("Baidu_"):
            return self._try_baidu_api(api_id, image_path, is_first)
        elif api_id.startswith("Tencent_"):
            return self._try_tencent_api(api_id, image_path, is_first)
        elif api_id == "默认":
            return self._try_default_api(image_path, user_selection)
        return None

    def _try_baidu_api(self, api_id: str, image_path: str, is_first: bool) -> Optional[str]:
        """Try Baidu OCR API."""
        if not self._config.has_baidu_ocr_credentials():
            if is_first:
                self.ocr_error.emit("OCR 提示", "百度OCR配置缺失，跳过百度接口。")
                logging.warning("Baidu OCR keys missing, skipping Baidu.")
            return None

        api_type = api_id.split('_', 1)[1]
        result = use_baidu_ocr(
            image_path,
            self._config.api_key,
            self._config.secret_key,
            api_type=api_type
        )

        if result == 'LIMIT_REACHED_ERROR':
            if api_id in ['Baidu_table', 'Baidu_formula']:
                return "用量已达本月上限"
            return None
        elif result == 'NO_PERMISSION_ERROR':
            if api_id in ['Baidu_table', 'Baidu_formula']:
                return f"无权限访问 {api_type} 接口，请检查百度AI控制台"
            return None
        elif result is not None:
            return result
        return None

    def _try_tencent_api(self, api_id: str, image_path: str, is_first: bool) -> Optional[str]:
        """Try Tencent OCR API."""
        if not self._config.has_tencent_ocr_credentials():
            if is_first:
                self.ocr_error.emit("OCR 提示", "腾讯OCR配置缺失，跳过腾讯接口。")
                logging.warning("Tencent OCR keys missing, skipping Tencent.")
            return None

        tencent_api_map = {
            'Tencent_general_basic': 'general_basic',
            'Tencent_general_accurate': 'general_accurate',
            'Tencent_table': 'table',
            'Tencent_formula': 'formula'
        }
        api_type = tencent_api_map.get(api_id, 'general_basic')
        result = use_tencent_ocr(
            image_path,
            self._config.tencent_secret_id,
            self._config.tencent_secret_key,
            api_type=api_type
        )

        if result == 'LIMIT_REACHED_ERROR':
            return "用量已达本月上限"
        elif result == 'NO_PERMISSION_ERROR':
            return f"无权限访问腾讯 {api_type} 接口，请检查腾讯云控制台"
        elif result is not None:
            return result
        return None

    def _try_default_api(self, image_path: str, user_selection: str) -> Optional[str]:
        """Try default (OneOCR) API."""
        # If falling back from cloud API, update selection
        if user_selection.startswith("Baidu_") or user_selection.startswith("Tencent_"):
            new_selection = "默认"
            logging.info(f"OCR ('{user_selection}') unavailable. Switching to '{new_selection}'.")
            self._config.last_ocr_selection = new_selection
            self._config.save()
            self.ocr_selection_changed.emit(new_selection)

        return use_oneocr(image_path)

    def _try_default_table_api(self, image_path: str) -> Optional[str]:
        """Try default (OneOCR) table recognition API."""
        return use_oneocr_table(image_path)
