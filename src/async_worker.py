# async_worker.py
"""
Translation token prewarming helper.

历史上此模块包含 AsyncWorker / OCRTranslationPipeline 两个基于 asyncio 的类，
但实际的 OCR/翻译流程已由 OCRManager / TranslationManager 直接用线程实现，
那两个类已无任何引用，故移除，仅保留仍被 OCRManager 使用的预热函数。
"""

import logging


def prewarm_translation_tokens():
    """
    Prewarm translation tokens for faster response.
    Can be called in parallel with OCR.
    """
    from .translate import get_edge_token
    try:
        get_edge_token(force_new=False)
        logging.debug("Translation tokens prewarmed")
    except Exception as e:
        logging.warning(f"Failed to prewarm translation tokens: {e}")
