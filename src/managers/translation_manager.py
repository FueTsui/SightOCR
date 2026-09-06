# translation_manager.py
"""
TranslationManager for managing translation requests with fallback.
Extracted from MainWindow to reduce its size.
"""

import itertools
import logging
import threading
import time
from typing import Optional

from PySide6.QtCore import QObject, Signal

from ..translate import bing_trans, baidu_trans, tencent_trans
from .config_manager import ConfigManager


class TranslationManager(QObject):
    """
    Manages translation requests with fallback chain.

    Usage:
        trans_mgr = TranslationManager()
        trans_mgr.translation_completed.connect(on_translation_done)
        trans_mgr.translation_error.connect(on_translation_error)
        trans_mgr.translate("Hello", "auto", "zh")
    """

    # Emitted when translation completes (translated_text)
    translation_completed = Signal(str)

    # Emitted when an error occurs (title, message)
    translation_error = Signal(str, str)

    # Emitted when translation source should be updated in UI
    translation_selection_changed = Signal(str)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._config = ConfigManager.instance()
        self._request_counter = itertools.count()
        self._latest_request_id = -1
        self._request_lock = threading.Lock()
        self._warmup_lock = threading.Lock()
        self._warmup_started = False
        self._last_warmup_at = 0.0
        self._keepalive_stop = threading.Event()
        self._keepalive_thread = None

    def translate(
        self,
        text: str,
        from_lang: str = 'auto',
        to_lang: str = 'zh',
        trace_label: Optional[str] = None
    ):
        """
        Translate text in a background thread.

        Args:
            text: Text to translate
            from_lang: Source language code
            to_lang: Target language code
        """
        if not text or not text.strip():
            self.translation_error.emit("翻译提示", "没有可翻译的文本。")
            return

        request_id = next(self._request_counter)
        with self._request_lock:
            self._latest_request_id = request_id

        if trace_label:
            logging.info(f"[perf:{trace_label}] Translation request accepted")

        threading.Thread(
            target=self._run_translation,
            args=(text.strip(), from_lang, to_lang, request_id, trace_label),
            daemon=True,
            name="TranslationWorker"
        ).start()

    def _run_translation(
        self,
        text: str,
        from_lang: str,
        to_lang: str,
        request_id: int,
        trace_label: Optional[str] = None
    ):
        """Execute translation with fallback logic."""
        try:
            started_at = time.perf_counter()
            translation_source = self._config.last_translate_selection
            translated = None
            if trace_label:
                logging.info(
                    f"[perf:{trace_label}] Translation worker started; source={translation_source} "
                    f"chars={len(text)}"
                )

            if translation_source == "Baidu":
                translated = self._try_baidu(text, from_lang, to_lang)
            elif translation_source == "Tencent":
                translated = self._try_tencent(text, from_lang, to_lang)

            # Fallback to default if selected source failed
            if translated is None:
                if translation_source in ["Baidu", "Tencent"]:
                    new_selection = "默认"
                    logging.info(f"{translation_source} Translate unavailable. Switching to '{new_selection}'.")
                    self._config.last_translate_selection = new_selection
                    self._config.save()
                    self.translation_selection_changed.emit(new_selection)

                translated = bing_trans(text, from_lang, to_lang)

            # 丢弃过时的翻译结果（用户已发起新的翻译请求）
            if not self._is_request_current(request_id):
                if trace_label:
                    elapsed_ms = (time.perf_counter() - started_at) * 1000
                    logging.debug(
                        f"[perf:{trace_label}] Discarding stale translation result; "
                        f"duration={elapsed_ms:.1f}ms request={request_id}"
                    )
                else:
                    logging.debug(
                        f"Discarding stale translation result (request {request_id}, "
                        f"latest {self._latest_request_id})"
                    )
                return

            if translated:
                if trace_label:
                    elapsed_ms = (time.perf_counter() - started_at) * 1000
                    logging.info(
                        f"[perf:{trace_label}] Translation completed; "
                        f"duration={elapsed_ms:.1f}ms chars={len(translated)}"
                    )
                self.translation_completed.emit(translated)
            else:
                if trace_label:
                    elapsed_ms = (time.perf_counter() - started_at) * 1000
                    logging.warning(
                        f"[perf:{trace_label}] Translation failed with empty result; "
                        f"duration={elapsed_ms:.1f}ms"
                    )
                self.translation_error.emit("翻译提示", "所有翻译接口均失败，请检查网络。")

        except Exception as e:
            if trace_label:
                elapsed_ms = (time.perf_counter() - started_at) * 1000
                logging.error(
                    f"[perf:{trace_label}] Translation crashed after {elapsed_ms:.1f}ms: {e}",
                    exc_info=True
                )
            logging.error(f"Translation error: {e}", exc_info=True)
            self.translation_error.emit("翻译提示", f"翻译时发生未知错误: {e}")

    def _try_baidu(self, text: str, from_lang: str, to_lang: str) -> Optional[str]:
        """Try Baidu translation."""
        if not self._config.has_baidu_trans_credentials():
            logging.warning("Baidu translate config missing")
            self.translation_error.emit("翻译提示", "百度翻译配置缺失，自动切换到默认接口。")
            return None

        translated = baidu_trans(
            text,
            self._config.baidu_trans_appid,
            self._config.baidu_trans_appkey,
            from_lang,
            to_lang
        )

        if not translated:
            logging.warning("Baidu translation failed")
            self.translation_error.emit("翻译提示", "百度翻译失败，已自动切换到默认接口。")
            return None

        return translated

    def _try_tencent(self, text: str, from_lang: str, to_lang: str) -> Optional[str]:
        """Try Tencent translation."""
        if not self._config.has_tencent_trans_credentials():
            logging.warning("Tencent translate config missing")
            self.translation_error.emit("翻译提示", "腾讯翻译配置缺失，自动切换到默认接口。")
            return None

        translated = tencent_trans(
            text,
            self._config.tencent_trans_secret_id,
            self._config.tencent_trans_secret_key,
            from_lang,
            to_lang
        )

        if not translated:
            logging.warning("Tencent translation failed")
            self.translation_error.emit("翻译提示", "腾讯翻译失败，已自动切换到默认接口。")
            return None

        return translated

    def prepare_hotkey_translation(self, trace_label: Optional[str] = None):
        """Directly start Bing warm-up when translation is about to be triggered."""
        if self._config.last_translate_selection != "默认":
            return
        self.warm_up_cache(trace_label=trace_label)

    def start_keepalive(self):
        """启动翻译保活线程。

        线程自带门控：仅当默认翻译启用时才真正发起保活请求，因此可在启动时
        无条件启动一次，用户后续切换翻译源也无需重启线程。
        """
        if self._keepalive_thread and self._keepalive_thread.is_alive():
            return
        self._keepalive_stop.clear()
        self._keepalive_thread = threading.Thread(
            target=self._keepalive_loop,
            daemon=True,
            name="TranslationKeepAlive"
        )
        self._keepalive_thread.start()
        logging.debug("Translation keepalive thread started")

    def stop_keepalive(self):
        """请求停止翻译保活线程（优雅退出）。"""
        self._keepalive_stop.set()

    def _keepalive_loop(self):
        """周期性保持 Edge token 与翻译连接处于热状态。"""
        from ..translate import keep_edge_warm, EDGE_KEEPALIVE_INTERVAL

        # 先 wait 再工作：避免与启动时的 warm_up_cache 重叠，且支持随时被唤醒退出
        while not self._keepalive_stop.wait(EDGE_KEEPALIVE_INTERVAL):
            try:
                if self._config.last_translate_selection != "默认":
                    continue
                keep_edge_warm()
            except Exception as e:
                logging.debug(f"Translation keepalive loop error: {e}")

    def warm_up_cache(self, force: bool = False, trace_label: Optional[str] = None):
        """Warm up translation caches in background."""
        if self._config.last_translate_selection != "默认":
            if trace_label:
                logging.debug(
                    f"[perf:{trace_label}] Skipping Edge cache warm-up because default translation is not selected"
                )
            else:
                logging.debug("Skipping Edge cache warm-up because default translation is not selected")
            return

        with self._warmup_lock:
            now = time.time()
            cache_still_fresh = now - self._last_warmup_at < 60 * 6
            if self._warmup_started and not force:
                if trace_label:
                    logging.debug(f"[perf:{trace_label}] Edge cache warm-up already in progress")
                else:
                    logging.debug("Edge cache warm-up already in progress")
                return
            if cache_still_fresh and not force:
                if trace_label:
                    logging.debug(
                        f"[perf:{trace_label}] Skipping Edge cache warm-up because recent warm-up is still fresh"
                    )
                else:
                    logging.debug("Skipping Edge cache warm-up because recent warm-up is still fresh")
                return
            self._warmup_started = True

        threading.Thread(
            target=self._warm_up_bing_cache,
            args=(force, trace_label),
            daemon=True,
            name="TranslationWarmUp"
        ).start()

    def _warm_up_bing_cache(self, force: bool = False, trace_label: Optional[str] = None):
        """Warm up Edge translation token cache."""
        from ..translate import get_edge_token
        started_at = time.perf_counter()
        try:
            if trace_label:
                logging.info(f"[perf:{trace_label}] Edge token warm-up started")
            else:
                logging.info("Warming up Edge Translate token cache...")
            token = get_edge_token(force_new=force)
            if not token:
                raise RuntimeError("Edge token warm-up returned empty result")
            with self._warmup_lock:
                self._last_warmup_at = time.time()
                self._warmup_started = False
            elapsed_ms = (time.perf_counter() - started_at) * 1000
            if trace_label:
                logging.info(f"[perf:{trace_label}] Edge token warm-up ready; duration={elapsed_ms:.1f}ms")
            else:
                logging.info("Edge Translate token cache warmed up")
        except Exception as e:
            with self._warmup_lock:
                self._warmup_started = False
            elapsed_ms = (time.perf_counter() - started_at) * 1000
            if trace_label:
                logging.warning(
                    f"[perf:{trace_label}] Edge token warm-up failed after {elapsed_ms:.1f}ms: {e}"
                )
            else:
                logging.warning(f"Failed to warm up Edge cache: {e}")

    def _is_request_current(self, request_id: int) -> bool:
        """Return whether the request is still the latest one."""
        with self._request_lock:
            return request_id == self._latest_request_id
