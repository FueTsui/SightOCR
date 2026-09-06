# perf_trace.py
"""
翻译流水线性能追踪器。

从 MainWindow 中抽离，集中管理一次「OCR→翻译」流程的性能里程碑日志，
使主窗口不再承担计时/计数/日志格式化职责（A3：拆分 god class）。

线程模型：所有方法均预期在 Qt 主线程上调用（与原 MainWindow 内联实现一致），
因此不额外加锁。trace 标识符随调用透传到工作线程仅用于日志关联，不共享可变状态。
"""

import logging
import time
from typing import Optional


class TranslateTracer:
    """跟踪单条「OCR+翻译」流水线的性能埋点。

    用法：
        tracer = TranslateTracer()
        tracer.start("hotkey")
        label = tracer.active_id           # 透传给工作线程做日志关联
        tracer.log("dispatching translation")
        tracer.finish("completed", "chars=42")
    """

    # OCR/翻译流程中代表「终态」的错误文案，命中即结束当前 trace
    _TERMINAL_MESSAGES = (
        "没有可翻译的文本。",
        "未选择有效的截屏区域。",
        "所有OCR接口均未能识别到内容，请重试。",
        "所有翻译接口均失败，请检查网络。",
    )

    def __init__(self):
        self._counter = 0
        self._active = None

    @property
    def is_active(self) -> bool:
        """当前是否有进行中的 trace。"""
        return self._active is not None

    @property
    def active_id(self) -> Optional[str]:
        """返回当前 trace 的标识符，无活动 trace 时返回 None。"""
        return self._active['id'] if self._active else None

    def start(self, origin: str) -> dict:
        """开始一条新的流水线 trace；若已有活动 trace 则记录其被替换。"""
        if self._active:
            previous = self._active
            elapsed_ms = (time.perf_counter() - previous['started_at']) * 1000
            logging.info(
                f"[perf:{previous['id']}] translate pipeline replaced; elapsed={elapsed_ms:.1f}ms"
            )

        self._counter += 1
        trace = {
            'id': f"T{self._counter:04d}",
            'origin': origin,
            'started_at': time.perf_counter(),
        }
        self._active = trace
        logging.info(f"[perf:{trace['id']}] translate pipeline triggered via {origin}")
        return trace

    def log(self, message: str, level: int = logging.INFO):
        """记录当前活动 trace 的一个里程碑；无活动 trace 时静默忽略。"""
        if not self._active:
            return
        elapsed_ms = (time.perf_counter() - self._active['started_at']) * 1000
        logging.log(
            level,
            f"[perf:{self._active['id']}] {message}; elapsed={elapsed_ms:.1f}ms"
        )

    def finish(self, status: str, detail: str = ""):
        """结束并清空当前 trace。"""
        if not self._active:
            return
        elapsed_ms = (time.perf_counter() - self._active['started_at']) * 1000
        suffix = f"; {detail}" if detail else ""
        logging.info(
            f"[perf:{self._active['id']}] translate pipeline {status}; "
            f"total={elapsed_ms:.1f}ms{suffix}"
        )
        self._active = None

    def maybe_finish_on_error(self, title: str, message: str):
        """在终态 OCR/翻译错误时结束当前 trace。"""
        if not self._active:
            return
        if title == "OCR 初始化失败" or any(text in message for text in self._TERMINAL_MESSAGES):
            self.finish("failed", f"{title}: {message}")
