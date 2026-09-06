import os
import logging
import ctypes
from ctypes import (
    Structure,
    POINTER,
    c_char_p,
    c_uint32,
    c_uint64,
    c_float,
    c_void_p,
    c_ubyte,
    byref,
    addressof,
    string_at,
    sizeof,
    memmove,
)
import requests
import base64
import time
import threading
import atexit
from .utils import resource_path

# 尝试导入 Pillow 库
try:
    from PIL import Image
except ImportError:
    logging.error("未找到 Pillow 库，OneOCR 功能将不可用")
    Image = None

# 表格识别重量级依赖（numpy/scipy/cv2/sklearn）延迟加载
# 这些库在 PyInstaller 打包后导入耗时较长，延迟到首次表格识别时才加载
np = None
hcluster = None
minimum_filter1d = None
cv2 = None
_DBSCAN = None
_HAS_TABLE_DEPS = None  # None=未检测, True/False=检测结果
_HAS_DBSCAN = None
_table_deps_init_lock = threading.Lock()


def _ensure_table_deps():
    """延迟加载表格识别依赖库，仅在首次表格识别时执行。"""
    global np, hcluster, minimum_filter1d, cv2, _DBSCAN, _HAS_TABLE_DEPS, _HAS_DBSCAN

    if _HAS_TABLE_DEPS is not None:
        return

    with _table_deps_init_lock:
        if _HAS_TABLE_DEPS is not None:
            return

        try:
            import numpy as _numpy
            from scipy.cluster import hierarchy as _hierarchy
            from scipy.ndimage import minimum_filter1d as _mf1d
            np = _numpy
            hcluster = _hierarchy
            minimum_filter1d = _mf1d
            _HAS_TABLE_DEPS = True
        except ImportError:
            _HAS_TABLE_DEPS = False
            logging.warning("numpy/scipy 未安装，本地表格识别功能将不可用")
            _HAS_DBSCAN = False
            return

        try:
            import cv2 as _cv2
            try:
                _cv2.ocl.setUseOpenCL(False)
            except Exception:
                pass
            cv2 = _cv2
        except ImportError:
            pass  # cv2 保持 None，已有的 if cv2 is None 检查会正确处理

        try:
            from sklearn.cluster import DBSCAN
            _DBSCAN = DBSCAN
            _HAS_DBSCAN = True
        except ImportError:
            _HAS_DBSCAN = False

# 定义固定的路径常量
ONEOCR_DLL_PATH = resource_path(os.path.join("resources", "oneocr", "oneocr.dll"))
ONNXRUNTIME_DLL_PATH = resource_path(os.path.join("resources", "oneocr", "onnxruntime.dll"))
ONEOCR_MODEL_PATH = resource_path(os.path.join("resources", "oneocr", "oneocr.onemodel"))

# OneOCR 许可证密钥
OCR_LICENSE_KEY = "kj)TGtrK>f]b[Piow.gU+nC@s\"\"\"\"\"\"4"

# --- C 类型定义 (用于 oneocr.dll) ---

class Point(Structure):
    """坐标点结构体"""
    _fields_ = [("x", c_float), ("y", c_float)]

class BoundingBox(Structure):
    """文本行边界框结构体"""
    _fields_ = [
        ("TopLeft", Point),
        ("TopRight", Point),
        ("BottomRight", Point),
        ("BottomLeft", Point),
    ]

class ImageInfo(Structure):
    """对应 C++ 中的 ImageInfo 结构体"""
    _fields_ = [
        ("type", c_uint32),
        ("width", c_uint32),
        ("height", c_uint32),
        ("stride", c_uint64),
        ("dataPointer", c_uint64),
    ]

# 定义各种句柄类型
OCRInitOptionsHandle = c_void_p
OCRPipelineHandle = c_void_p
OCRProcessOptionsHandle = c_void_p
OCRResultHandle = c_void_p
OCRLineHandle = c_void_p
LPCCH = c_char_p

# 全局变量用于缓存Access Token和过期时间（线程安全）
_token_cache_lock = threading.Lock()
access_token_cache = {
    'token': None,
    'expires_at': 0
}

# OneOCR 全局实例和初始化状态
_oneocr_instance = None
oneocr_initialized = False
oneocr_init_lock = threading.Lock()

# SSL 验证配置（生产环境必须启用）
SSL_VERIFY = True

# HTTP 会话（连接池复用）
_http_session = None
_http_session_lock = threading.Lock()


def _get_http_session():
    """获取或创建 HTTP 会话（线程安全，连接池复用）"""
    global _http_session
    if _http_session is None:
        with _http_session_lock:
            if _http_session is None:
                _http_session = requests.Session()
                # 配置连接池大小
                adapter = requests.adapters.HTTPAdapter(
                    pool_connections=5,
                    pool_maxsize=10,
                    max_retries=1
                )
                _http_session.mount('https://', adapter)
                _http_session.mount('http://', adapter)
    return _http_session


def clear_sensitive_cache():
    """清理敏感的令牌缓存和 OneOCR 资源（程序退出时调用）"""
    global access_token_cache, _http_session, _oneocr_instance, oneocr_initialized
    with _token_cache_lock:
        access_token_cache['token'] = None
        access_token_cache['expires_at'] = 0
    # 关闭 HTTP 会话
    with _http_session_lock:
        if _http_session is not None:
            try:
                _http_session.close()
                logging.debug("OCR HTTP 会话已关闭")
            except Exception as e:
                logging.warning(f"关闭 OCR HTTP 会话时出错: {e}")
            finally:
                _http_session = None
    # 清理 OneOCR 实例
    with oneocr_init_lock:
        if _oneocr_instance is not None:
            try:
                _oneocr_instance.close()
                logging.debug("OneOCR 实例已关闭")
            except Exception as e:
                logging.warning(f"关闭 OneOCR 实例时出错: {e}")
            finally:
                _oneocr_instance = None
                oneocr_initialized = False
    logging.debug("OCR 令牌缓存已清理")


# 注册 atexit 钩子，确保程序退出时清理资源
atexit.register(clear_sensitive_cache)


class OneOCR:
    """
    封装了 oneocr.dll 功能的类，用于执行 OCR 识别。
    线程安全：recognize() 方法使用锁保护。
    """
    def __init__(self, model_path=None):
        if model_path is None:
            model_path = ONEOCR_MODEL_PATH

        # 线程锁，保护 DLL 调用
        self._recognize_lock = threading.Lock()

        # 初始化 COM (Windows WIC 需要)
        self.ole32 = ctypes.windll.ole32
        self.ole32.CoInitialize(None)

        self._validate_paths(model_path)

        # 预加载 onnxruntime.dll
        try:
            ctypes.CDLL(ONNXRUNTIME_DLL_PATH)
        except Exception as e:
            self.ole32.CoUninitialize()
            raise RuntimeError(f"加载 onnxruntime.dll 失败: {e}")

        self.dll = ctypes.CDLL(ONEOCR_DLL_PATH)
        self.model_path = model_path

        self.initOptionsHandle = OCRInitOptionsHandle()
        self.pipelineHandle = OCRPipelineHandle()
        self.processOptionsHandle = OCRProcessOptionsHandle()

        self._define_function_signatures()
        self._initialize_ocr_engine()

    def _validate_paths(self, model_path):
        """检查所有必需的文件是否存在"""
        if not os.path.exists(ONEOCR_DLL_PATH):
            raise FileNotFoundError(f"未找到 oneocr.dll: {ONEOCR_DLL_PATH}")
        if not os.path.exists(ONNXRUNTIME_DLL_PATH):
            raise FileNotFoundError(f"未找到 onnxruntime.dll: {ONNXRUNTIME_DLL_PATH}")
        if not os.path.exists(model_path):
            raise FileNotFoundError(f"未找到模型文件: {model_path}")

    def _define_function_signatures(self):
        """定义 DLL 函数签名"""
        self.dll.CreateOcrInitOptions.argtypes = [POINTER(OCRInitOptionsHandle)]
        self.dll.CreateOcrInitOptions.restype = c_uint32

        self.dll.OcrInitOptionsSetUseModelDelayLoad.argtypes = [OCRInitOptionsHandle, ctypes.c_char]
        self.dll.OcrInitOptionsSetUseModelDelayLoad.restype = c_uint32

        self.dll.CreateOcrPipeline.argtypes = [c_char_p, c_char_p, OCRInitOptionsHandle, POINTER(OCRPipelineHandle)]
        self.dll.CreateOcrPipeline.restype = c_uint32

        self.dll.CreateOcrProcessOptions.argtypes = [POINTER(OCRProcessOptionsHandle)]
        self.dll.CreateOcrProcessOptions.restype = c_uint32

        self.dll.OcrProcessOptionsSetMaxRecognitionLineCount.argtypes = [OCRProcessOptionsHandle, c_uint32]
        self.dll.OcrProcessOptionsSetMaxRecognitionLineCount.restype = c_uint32

        self.dll.OcrProcessOptionsSetResizeResolution.argtypes = [OCRProcessOptionsHandle, c_uint32, c_uint32]
        self.dll.OcrProcessOptionsSetResizeResolution.restype = c_uint32

        self.dll.RunOcrPipeline.argtypes = [OCRPipelineHandle, POINTER(ImageInfo), OCRProcessOptionsHandle, POINTER(OCRResultHandle)]
        self.dll.RunOcrPipeline.restype = c_uint32

        self.dll.GetOcrLineCount.argtypes = [OCRResultHandle, POINTER(c_uint64)]
        self.dll.GetOcrLineCount.restype = c_uint32

        self.dll.GetOcrLine.argtypes = [OCRResultHandle, c_uint64, POINTER(OCRLineHandle)]
        self.dll.GetOcrLine.restype = c_uint32

        self.dll.GetOcrLineContent.argtypes = [OCRLineHandle, POINTER(LPCCH)]
        self.dll.GetOcrLineContent.restype = c_uint32

        self.dll.GetOcrLineBoundingBox.argtypes = [OCRLineHandle, POINTER(POINTER(BoundingBox))]
        self.dll.GetOcrLineBoundingBox.restype = c_uint32

        self.dll.ReleaseOcrResult.argtypes = [OCRResultHandle]
        self.dll.ReleaseOcrResult.restype = c_uint32

        self.dll.ReleaseOcrProcessOptions.argtypes = [OCRProcessOptionsHandle]
        self.dll.ReleaseOcrProcessOptions.restype = c_uint32

        self.dll.ReleaseOcrPipeline.argtypes = [OCRPipelineHandle]
        self.dll.ReleaseOcrPipeline.restype = c_uint32

        self.dll.ReleaseOcrInitOptions.argtypes = [OCRInitOptionsHandle]
        self.dll.ReleaseOcrInitOptions.restype = c_uint32

    def _initialize_ocr_engine(self):
        """初始化 OCR 引擎"""
        def check_result(result, func_name):
            if result != 0:
                raise RuntimeError(f"{func_name} 失败, 错误码: {result}")

        check_result(self.dll.CreateOcrInitOptions(byref(self.initOptionsHandle)), "CreateOcrInitOptions")
        check_result(self.dll.OcrInitOptionsSetUseModelDelayLoad(self.initOptionsHandle, 0), "OcrInitOptionsSetUseModelDelayLoad")

        model_path_bytes = self.model_path.encode('mbcs')
        license_key_bytes = OCR_LICENSE_KEY.encode('mbcs')
        check_result(self.dll.CreateOcrPipeline(model_path_bytes, license_key_bytes, self.initOptionsHandle, byref(self.pipelineHandle)), "CreateOcrPipeline")

        check_result(self.dll.CreateOcrProcessOptions(byref(self.processOptionsHandle)), "CreateOcrProcessOptions")
        check_result(self.dll.OcrProcessOptionsSetMaxRecognitionLineCount(self.processOptionsHandle, 1000), "OcrProcessOptionsSetMaxRecognitionLineCount")
        check_result(self.dll.OcrProcessOptionsSetResizeResolution(self.processOptionsHandle, 1152, 768), "OcrProcessOptionsSetResizeResolution")

    def recognize(self, image_path: str, get_boxes: bool = False):
        """
        对单个图像文件执行 OCR。
        此方法是线程安全的。
        get_boxes: 为 True 时返回包含文本和边界框的列表，否则返回纯文本字符串。
        """
        if Image is None:
            return "错误: Pillow 库未安装"

        # 图像预处理（在锁外进行，提高并发性能）
        try:
            with Image.open(image_path) as img:
                if img.mode != 'RGBA':
                    img = img.convert('RGBA')

                width, height = img.size

                # 确保图像尺寸至少为 50x50
                if width < 50 or height < 50:
                    new_width = max(width, 50)
                    new_height = max(height, 50)
                    padded_img = Image.new('RGBA', (new_width, new_height), 'white')
                    padded_img.paste(img, (0, 0))
                    img = padded_img
                    width, height = new_width, new_height

                # RGBA 转 BGRA（使用 PIL 通道交换，避免逐字节循环）
                r, g, b, a = img.split()
                img_bgra = Image.merge('RGBA', (b, g, r, a))
                bgra_data = img_bgra.tobytes()
                c_bgra_data = (c_ubyte * len(bgra_data)).from_buffer_copy(bgra_data)

        except FileNotFoundError:
            return f"错误: 图像文件 '{image_path}' 不存在"
        except Exception as e:
            return f"处理图像时发生错误: {e}"

        image_info = ImageInfo(
            type=3,
            width=width,
            height=height,
            stride=width * 4,
            dataPointer=addressof(c_bgra_data)
        )

        # DLL 调用需要线程保护
        with self._recognize_lock:
            result_handle = OCRResultHandle()
            try:
                result = self.dll.RunOcrPipeline(
                    self.pipelineHandle, byref(image_info),
                    self.processOptionsHandle, byref(result_handle)
                )

                if result != 0:
                    return f"OCR 识别过程失败, 错误码: {result}"

                if get_boxes:
                    return self._get_lines_with_boxes_from_result(result_handle)
                text = self._get_text_from_result(result_handle)
                return text
            finally:
                # 确保结果句柄被释放
                if result_handle and result_handle.value:
                    self.dll.ReleaseOcrResult(result_handle)

    def _get_text_from_result(self, result_handle: OCRResultHandle) -> str:
        """从 OCR 结果句柄中提取所有文本行"""
        line_count = c_uint64(0)
        if self.dll.GetOcrLineCount(result_handle, byref(line_count)) != 0:
            return ""

        output_lines = []
        for i in range(line_count.value):
            line_handle = OCRLineHandle()
            if self.dll.GetOcrLine(result_handle, i, byref(line_handle)) != 0:
                continue

            line_content_ptr = LPCCH()
            if self.dll.GetOcrLineContent(line_handle, byref(line_content_ptr)) != 0:
                continue

            line_text = string_at(line_content_ptr).decode('utf-8', errors='ignore')
            output_lines.append(line_text)

        return "\n".join(output_lines)

    def _get_lines_with_boxes_from_result(self, result_handle: OCRResultHandle) -> list:
        """从 OCR 结果句柄中提取所有文本行及其边界框"""
        line_count = c_uint64(0)
        if self.dll.GetOcrLineCount(result_handle, byref(line_count)) != 0:
            return []

        output_lines = []
        for i in range(line_count.value):
            line_handle = OCRLineHandle()
            if self.dll.GetOcrLine(result_handle, i, byref(line_handle)) != 0:
                continue

            line_content_ptr = LPCCH()
            if self.dll.GetOcrLineContent(line_handle, byref(line_content_ptr)) != 0:
                continue
            line_text = string_at(line_content_ptr).decode('utf-8', errors='ignore')

            bbox_ptr = POINTER(BoundingBox)()
            if self.dll.GetOcrLineBoundingBox(line_handle, byref(bbox_ptr)) == 0 and bbox_ptr:
                original_bbox = bbox_ptr.contents
                copied_bbox = BoundingBox()
                memmove(addressof(copied_bbox), addressof(original_bbox), sizeof(BoundingBox))
                output_lines.append({'text': line_text, 'box': copied_bbox})

        return output_lines

    def close(self):
        """释放所有 OCR 相关的句柄和资源"""
        if hasattr(self, 'dll'):
            if hasattr(self, 'processOptionsHandle') and self.processOptionsHandle and self.processOptionsHandle.value:
                self.dll.ReleaseOcrProcessOptions(self.processOptionsHandle)
            if hasattr(self, 'pipelineHandle') and self.pipelineHandle and self.pipelineHandle.value:
                self.dll.ReleaseOcrPipeline(self.pipelineHandle)
            if hasattr(self, 'initOptionsHandle') and self.initOptionsHandle and self.initOptionsHandle.value:
                self.dll.ReleaseOcrInitOptions(self.initOptionsHandle)

        if hasattr(self, 'ole32'):
            self.ole32.CoUninitialize()


def _get_oneocr_instance():
    """获取或创建 OneOCR 全局实例（线程安全，单例模式）"""
    global _oneocr_instance, oneocr_initialized
    if _oneocr_instance is not None:
        return _oneocr_instance

    with oneocr_init_lock:
        if _oneocr_instance is None:
            _oneocr_instance = OneOCR()
            oneocr_initialized = True
            logging.info("OneOCR 引擎初始化成功")
    return _oneocr_instance


def init_oneocr():
    """
    初始化 OneOCR 引擎。可在启动时调用以预初始化。
    """
    global oneocr_initialized, _oneocr_instance
    with oneocr_init_lock:
        if oneocr_initialized:
            return

        try:
            logging.debug(f"正在初始化 OneOCR DLL: {ONEOCR_DLL_PATH}")
            _oneocr_instance = OneOCR()
            oneocr_initialized = True
            logging.info("OneOCR 引擎初始化成功")

        except Exception as e:
            oneocr_initialized = False
            _oneocr_instance = None
            logging.error(f"OneOCR 初始化失败: {e}")
            raise e

def get_access_token(api_key, secret_key):
    """获取百度 API 访问令牌（线程安全）"""
    global access_token_cache
    current_time = time.time()

    # 先检查缓存（无锁快速路径）
    with _token_cache_lock:
        if access_token_cache['token'] and current_time < access_token_cache['expires_at']:
            return access_token_cache['token']

    url = "https://aip.baidubce.com/oauth/2.0/token"
    params = {
        "grant_type": "client_credentials",
        "client_id": api_key,
        "client_secret": secret_key
    }
    try:
        session = _get_http_session()
        response = session.post(url, params=params, verify=SSL_VERIFY,
                                proxies={'http': None, 'https': None}, timeout=10)
        response.raise_for_status()
        result = response.json()
        if 'error' in result:
            logging.error(f"Baidu token request failed: {result.get('error_description')}")
            return "API_KEY_ERROR"
        access_token = result.get("access_token")
        if not access_token:
            return "API_KEY_ERROR"
        # 有效期（秒），缺失或非法时回退到 30 分钟，避免 None 参与算术导致崩溃
        try:
            expires_in = int(result.get("expires_in") or 1800)
        except (TypeError, ValueError):
            expires_in = 1800
        # 线程安全地更新缓存
        with _token_cache_lock:
            access_token_cache['token'] = access_token
            access_token_cache['expires_at'] = current_time + expires_in - 60  # 提前60秒过期
        return access_token
    except requests.exceptions.Timeout:
        logging.error("Baidu token request timed out")
        return "NETWORK_ERROR"
    except Exception as e:
        logging.error(f"Failed to get Baidu access token: {e}")
        return "NETWORK_ERROR"

def get_file_content_as_base64(path):
    """
    获取文件的Base64编码
    :param path: 文件路径
    :return: Base64编码的文件内容，失败返回 None
    """
    try:
        with open(path, "rb") as f:
            content = base64.b64encode(f.read()).decode("utf8")
        return content
    except FileNotFoundError:
        logging.error(f"文件不存在: {path}")
        return None
    except Exception as e:
        logging.error(f"读取文件失败: {path}, 错误: {e}")
        return None

def _parse_table_result(result):
    """解析百度表格识别结果"""
    text_result = ""
    for table_info in result.get('tables_result', []):
        max_row = 0
        max_col = 0
        body = table_info.get('body', [])
        if not body:
            continue

        # 安全地获取最大行列数
        for cell in body:
            row_end = cell.get('row_end', 0)
            col_end = cell.get('col_end', 0)
            if row_end > max_row:
                max_row = row_end
            if col_end > max_col:
                max_col = col_end

        if max_row == 0 or max_col == 0:
            continue

        grid = [['' for _ in range(max_col + 1)] for _ in range(max_row + 1)]

        # 安全地填充表格
        for cell in body:
            row_start = cell.get('row_start', 0)
            col_start = cell.get('col_start', 0)
            words = cell.get('words', '')
            if 0 <= row_start <= max_row and 0 <= col_start <= max_col:
                grid[row_start][col_start] = words

        for row in grid:
            text_result += "\t".join(str(cell).replace('\n', ' ') for cell in row) + "\n"
        text_result += "\n"
    return text_result

def use_baidu_ocr(image_path, api_key, secret_key, api_type='accurate_basic'):
    """百度 OCR 识别"""
    access_token = get_access_token(api_key, secret_key)
    if access_token in ("API_KEY_ERROR", "NETWORK_ERROR"):
        logging.error(f"Baidu OCR: Failed to get access token ({access_token}).")
        return None

    api_endpoints = {
        'accurate_basic': 'accurate_basic',
        'accurate': 'accurate',
        'general_basic': 'general_basic',
        'general': 'general',
        'table': 'table',
        'formula': 'formula',
    }
    endpoint = api_endpoints.get(api_type, 'accurate_basic')
    url = f"https://aip.baidubce.com/rest/2.0/ocr/v1/{endpoint}?access_token={access_token}"

    image_base64 = get_file_content_as_base64(image_path)
    if image_base64 is None:
        logging.error(f"Baidu OCR: 无法读取图片文件: {image_path}")
        return None

    payload = {
        'image': image_base64,
        'detect_direction': 'false',
    }
    if api_type == 'formula':
        payload['disp_formula'] = 'true'
    elif endpoint not in ['table', 'formula']:
        payload['probability'] = 'false'
        payload['language_type'] = 'auto_detect'
        payload['paragraph'] = 'false'

    headers = {
        'Content-Type': 'application/x-www-form-urlencoded',
        'Accept': 'application/json'
    }

    try:
        session = _get_http_session()
        response = session.post(url, headers=headers, data=payload, timeout=10,
                                verify=SSL_VERIFY, proxies={'http': None, 'https': None})
        response.raise_for_status()
        result = response.json()

        if 'error_code' in result:
            error_code = result['error_code']
            error_msg = result.get('error_msg', 'Unknown API error')
            logging.error(f"Baidu OCR API error. Code: {error_code}, Message: {error_msg}, Log ID: {result.get('log_id')}")

            # Permission error
            if error_code == 6:
                return 'NO_PERMISSION_ERROR'

            # Limit errors (Daily, Monthly/Total)
            if error_code in [4, 17, 19]:
                return 'LIMIT_REACHED_ERROR'

            if error_code in [110, 111]:
                # 线程安全地清除令牌缓存
                with _token_cache_lock:
                    access_token_cache['token'] = None
                    access_token_cache['expires_at'] = 0
                logging.info("Baidu access token invalidated due to API error.")

            return None  # Trigger fallback for other API errors

        if api_type == 'table':
            return _parse_table_result(result)
        elif api_type == 'formula':
            formula_results = result.get('formula_result', [])
            return "\n\n".join([item.get('words', '') for item in formula_results]) if formula_results else ""
        else:
            words_result = result.get('words_result', [])
            return "\n".join([item.get('words', '') for item in words_result]) if words_result else ""
            
    except requests.exceptions.Timeout:
        logging.error("Baidu OCR request timed out.")
        return None
    except Exception as e:
        logging.error(f"Exception in use_baidu_ocr: {e}", exc_info=True)
        return None

def use_oneocr(image_path):
    """
    使用 OneOCR 识别图片中的文字。
    直接调用 oneocr.dll 进行 OCR 识别。
    """
    global _oneocr_instance, oneocr_initialized

    try:
        # 线程安全地获取或初始化 OneOCR 实例
        if _oneocr_instance is None:
            with oneocr_init_lock:
                if _oneocr_instance is None:
                    _oneocr_instance = OneOCR()
                    oneocr_initialized = True
                    logging.info("OneOCR 引擎初始化成功")

        logging.debug(f"开始OCR识别图片: {image_path}")

        # 调用 OneOCR 识别
        text_result = _oneocr_instance.recognize(image_path)

        # 检查是否返回错误信息
        if text_result and text_result.startswith("错误:"):
            logging.error(f"OneOCR 识别失败: {text_result}")
            return None

        if not text_result:
            logging.warning("OneOCR 返回空结果")
            return None

        logging.debug(f"OCR识别成功，结果长度: {len(text_result)}")
        return text_result

    except FileNotFoundError as e:
        logging.error(f"OneOCR 文件未找到: {e}")
        return None
    except RuntimeError as e:
        logging.error(f"OneOCR 运行时错误: {e}")
        return None
    except Exception as e:
        logging.error(f"OneOCR 调用失败: {e}")
        return None


# ==================== 本地表格识别 (基于 OneOCR) ====================

def _otsu_threshold(gray):
    """计算灰度图像的 Otsu 最佳二值化阈值（NumPy 向量化）"""
    hist = np.bincount(gray.ravel(), minlength=256).astype(np.float64)
    total = gray.size
    if total == 0:
        return 128

    bin_indices = np.arange(256, dtype=np.float64)
    sum_total = np.dot(bin_indices, hist)

    cum_weight = np.cumsum(hist)
    cum_sum = np.cumsum(bin_indices * hist)
    weight_fg = total - cum_weight

    # 避免除零
    valid = (cum_weight > 0) & (weight_fg > 0)
    mean_bg = np.where(valid, cum_sum / np.maximum(cum_weight, 1), 0)
    mean_fg = np.where(valid, (sum_total - cum_sum) / np.maximum(weight_fg, 1), 0)
    var_between = np.where(valid, cum_weight * weight_fg * (mean_bg - mean_fg) ** 2, 0)

    return int(np.argmax(var_between))


def _find_line_centers(proj, min_val, min_gap):
    """从 1D 投影信号中提取峰值位置（即网格线坐标）"""
    in_peak = False
    start = 0
    centers = []
    strengths = []  # 各峰值的信号积分，用于在相邻峰值冲突时保留更强的
    for i in range(len(proj)):
        if proj[i] >= min_val and not in_peak:
            in_peak = True
            start = i
        elif proj[i] < min_val and in_peak:
            in_peak = False
            center = (start + i) // 2
            strength = float(np.sum(proj[start:i]))
            if not centers or (center - centers[-1]) >= min_gap:
                centers.append(center)
                strengths.append(strength)
            elif strength > strengths[-1]:
                # 相邻峰值太近时保留信号更强的，避免弱噪声峰遮蔽真实网格线
                centers[-1] = center
                strengths[-1] = strength
    if in_peak:
        center = (start + len(proj)) // 2
        strength = float(np.sum(proj[start:]))
        if not centers or (center - centers[-1]) >= min_gap:
            centers.append(center)
            strengths.append(strength)
        elif strength > strengths[-1]:
            centers[-1] = center
            strengths[-1] = strength
    return centers


def _deskew_image(gray):
    """使用 Hough 线检测进行倾斜校正（需要 cv2）"""
    if cv2 is None:
        return gray
    try:
        edges = cv2.Canny(gray, 50, 150, apertureSize=3)
        lines = cv2.HoughLines(edges, 1, np.pi / 180,
                               threshold=min(gray.shape) // 4)
        if lines is None or len(lines) == 0:
            return gray

        angles = []
        for line in lines:
            theta = line[0][1]
            deg = np.degrees(theta) - 90
            if abs(deg) < 15:
                angles.append(deg)

        if not angles:
            return gray

        # 使用中位数 + MAD 过滤异常角度
        median_angle = np.median(angles)
        mad = np.median(np.abs(np.array(angles) - median_angle))
        if mad > 0:
            filtered = [a for a in angles if abs(a - median_angle) < 3 * mad]
            if filtered:
                median_angle = np.median(filtered)

        if abs(median_angle) < 1.5:
            return gray

        H, W = gray.shape
        center = (W // 2, H // 2)
        M = cv2.getRotationMatrix2D(center, median_angle, 1.0)
        return cv2.warpAffine(gray, M, (W, H), flags=cv2.INTER_LINEAR,
                              borderMode=cv2.BORDER_REPLICATE)
    except Exception:
        return gray


def _detect_table_grid_cv2(gray, H, W):
    """使用 OpenCV 检测表格网格线（多尺度形态学 + HoughLinesP 短线补充）"""
    gray = _deskew_image(gray)

    # 高斯模糊消除像素级噪声，提高二值化稳定性
    gray = cv2.GaussianBlur(gray, (3, 3), 0)

    binary = cv2.adaptiveThreshold(gray, 255, cv2.ADAPTIVE_THRESH_GAUSSIAN_C,
                                   cv2.THRESH_BINARY_INV, 15, 5)

    min_gap = max(min(H, W) // 50, 3)
    h_lines = []
    v_lines = []

    # === 多尺度形态学 ===
    # 大核(1/4)：检测贯穿全表的长网格线（高置信）
    # 小核(1/8)：检测仅跨若干行/列的短分隔线
    for kernel_frac, min_val_frac in [(4, 0.15), (8, 0.08)]:
        h_ks = max(W // kernel_frac, 10)
        h_k = cv2.getStructuringElement(cv2.MORPH_RECT, (h_ks, 1))
        h_m = cv2.morphologyEx(binary, cv2.MORPH_OPEN, h_k)

        v_ks = max(H // kernel_frac, 10)
        v_k = cv2.getStructuringElement(cv2.MORPH_RECT, (1, v_ks))
        v_m = cv2.morphologyEx(binary, cv2.MORPH_OPEN, v_k)

        hp = h_m.astype(np.float64).sum(axis=1) / 255.0
        vp = v_m.astype(np.float64).sum(axis=0) / 255.0

        h_mv = max(W * min_val_frac, 5)
        v_mv = max(H * min_val_frac, 5)

        for y in _find_line_centers(hp, h_mv, min_gap):
            if all(abs(y - e) > min_gap for e in h_lines):
                h_lines.append(y)
        for x in _find_line_centers(vp, v_mv, min_gap):
            if all(abs(x - e) > min_gap for e in v_lines):
                v_lines.append(x)

    # === HoughLinesP 补充检测极短分隔线 ===
    # 形态学投影会被长度不足区域的零值稀释，HoughLinesP 直接检测线段不受此限制
    min_v_len = max(H // 6, 20)
    min_h_len = max(W // 6, 20)
    edges = cv2.Canny(binary, 50, 150)
    lines_p = cv2.HoughLinesP(edges, 1, np.pi / 180, threshold=50,
                               minLineLength=min(min_v_len, min_h_len),
                               maxLineGap=5)

    if lines_p is not None:
        v_cands = []
        h_cands = []
        for lp in lines_p:
            x1, y1, x2, y2 = lp[0]
            dx, dy = abs(x2 - x1), abs(y2 - y1)
            # 近垂直线段
            if dx <= 3 and dy >= min_v_len:
                v_cands.append((x1 + x2) // 2)
            # 近水平线段
            elif dy <= 3 and dx >= min_h_len:
                h_cands.append((y1 + y2) // 2)

        # 聚合相近位置的片段，要求至少 2 个片段确认同一位置（排除文字笔画误检）
        for cands, lines_list in [(v_cands, v_lines), (h_cands, h_lines)]:
            if not cands:
                continue
            cands_sorted = sorted(cands)
            groups = [[cands_sorted[0]]]
            for pos in cands_sorted[1:]:
                if pos - groups[-1][-1] <= min_gap:
                    groups[-1].append(pos)
                else:
                    groups.append([pos])
            for group in groups:
                if len(group) >= 2:
                    mid = int(np.mean(group))
                    if all(abs(mid - e) > min_gap for e in lines_list):
                        lines_list.append(mid)

    h_lines.sort()
    v_lines.sort()

    if not h_lines and not v_lines:
        return [], []

    return h_lines, v_lines


def _detect_table_grid_numpy(gray, H, W):
    """使用 NumPy 检测表格网格线（多尺度回退方案）"""
    thresh = _otsu_threshold(gray)
    binary = (gray < thresh).astype(np.uint8)

    min_gap = max(min(H, W) // 50, 3)
    smooth_k = max(3, min(H, W) // 100)
    smooth_kernel = np.ones(smooth_k) / smooth_k
    h_lines = []
    v_lines = []

    # 多尺度检测：大核(1/4)检测长线 + 小核(1/8)检测短分隔线
    for kernel_frac, min_val_frac in [(4, 0.15), (8, 0.08)]:
        h_kernel = max(W // kernel_frac, 10)
        h_mask = minimum_filter1d(binary, size=h_kernel, axis=1)
        v_kernel = max(H // kernel_frac, 10)
        v_mask = minimum_filter1d(binary, size=v_kernel, axis=0)

        h_proj = h_mask.sum(axis=1).astype(np.float64)
        v_proj = v_mask.sum(axis=0).astype(np.float64)

        # 平滑投影信号，减少像素级噪声导致的峰值抖动
        h_proj = np.convolve(h_proj, smooth_kernel, mode='same')
        v_proj = np.convolve(v_proj, smooth_kernel, mode='same')

        h_mv = max(W * min_val_frac, 5)
        v_mv = max(H * min_val_frac, 5)

        for y in _find_line_centers(h_proj, h_mv, min_gap):
            if all(abs(y - e) > min_gap for e in h_lines):
                h_lines.append(y)
        for x in _find_line_centers(v_proj, v_mv, min_gap):
            if all(abs(x - e) > min_gap for e in v_lines):
                v_lines.append(x)

    h_lines.sort()
    v_lines.sort()

    if not h_lines and not v_lines:
        return [], []

    return h_lines, v_lines


def _detect_table_grid(image_path):
    """从图像中检测水平和垂直网格线"""
    try:
        with Image.open(image_path) as img:
            gray = np.array(img.convert('L'))
    except Exception:
        return [], []

    H, W = gray.shape

    if cv2 is not None:
        return _detect_table_grid_cv2(gray, H, W)
    else:
        return _detect_table_grid_numpy(gray, H, W)


def _find_gap_threshold(gaps, fallback):
    """在间距分布中寻找自然分割点（要求跳跃 > 2x 均值，避免因微小波动导致不稳定）"""
    if len(gaps) < 3:
        return fallback
    sorted_gaps = sorted(gaps)
    running_sum = sorted_gaps[0]
    for i in range(len(sorted_gaps) - 1):
        jump = sorted_gaps[i + 1] - sorted_gaps[i]
        avg_below = running_sum / (i + 1)
        # 要求跳跃至少为均值的 2 倍，且至少有 2 个样本在下方，降低对微小变化的敏感度
        if i >= 1 and avg_below > 0 and jump > avg_below * 2:
            return (sorted_gaps[i] + sorted_gaps[i + 1]) / 2.0
        running_sum += sorted_gaps[i + 1]
    return fallback


def _detect_column_splits(lines, h_lines):
    """从 OCR 文本块之间的水平间距中检测列分界位置"""
    if not lines or len(h_lines) < 2:
        return []

    num_rows = len(h_lines) - 1
    heights = [abs(l['box'].BottomLeft.y - l['box'].TopLeft.y) for l in lines]
    avg_h = max(float(np.mean(heights)), 1.0) if heights else 10.0

    row_groups = [[] for _ in range(num_rows)]
    for line in lines:
        cy = (line['box'].TopLeft.y + line['box'].BottomLeft.y) / 2.0
        best_row = -1
        for i in range(num_rows):
            if h_lines[i] <= cy <= h_lines[i + 1]:
                best_row = i
                break
        if best_row == -1:
            best_row = min(range(num_rows),
                          key=lambda i: abs((h_lines[i] + h_lines[i + 1]) / 2.0 - cy))
        row_groups[best_row].append(line)

    all_gaps = []
    for row_lines in row_groups:
        if len(row_lines) < 2:
            continue
        sorted_items = sorted(row_lines, key=lambda l: l['box'].TopLeft.x)
        for k in range(len(sorted_items) - 1):
            right_x = sorted_items[k]['box'].TopRight.x
            left_x = sorted_items[k + 1]['box'].TopLeft.x
            gap = left_x - right_x
            if gap > 0:
                all_gaps.append((gap, (right_x + left_x) / 2.0))

    if not all_gaps:
        return []

    gap_sizes = [g[0] for g in all_gaps]
    threshold = _find_gap_threshold(gap_sizes, avg_h * 0.8)
    all_gap_mids = [mid for size, mid in all_gaps if size > threshold]

    if not all_gap_mids:
        return []

    gap_arr = np.array(all_gap_mids).reshape(-1, 1)
    if len(gap_arr) == 1:
        return [float(gap_arr[0][0])]

    min_support = max(2, num_rows // 5)
    boundaries = []

    if _HAS_DBSCAN:
        # DBSCAN：对噪声点有鲁棒性，不需要预设聚类数
        db = _DBSCAN(eps=avg_h, min_samples=min_support)
        labels = db.fit_predict(gap_arr)
        for label in set(labels):
            if label == -1:  # 跳过噪声点
                continue
            mask = labels == label
            boundaries.append(float(np.mean(gap_arr[mask])))
    else:
        # 回退到层次聚类
        linkage_mat = hcluster.linkage(gap_arr, method='ward')
        clusters = hcluster.fcluster(linkage_mat, t=avg_h, criterion='distance')
        for cid in set(clusters):
            mask = clusters == cid
            if np.sum(mask) >= min_support:
                boundaries.append(float(np.mean(gap_arr[mask])))

    return sorted(boundaries)


def _detect_row_splits(lines):
    """从 OCR 文本块的垂直间距中检测行分界位置"""
    if len(lines) < 2:
        return []

    heights = [abs(l['box'].BottomLeft.y - l['box'].TopLeft.y) for l in lines]
    avg_h = max(float(np.mean(heights)), 1.0) if heights else 10.0

    if _HAS_DBSCAN:
        # 使用 DBSCAN 基于中心 y 坐标聚类（更稳健）
        centers_y = np.array(
            [(l['box'].TopLeft.y + l['box'].BottomLeft.y) / 2.0 for l in lines]
        ).reshape(-1, 1)
        db = _DBSCAN(eps=avg_h * 0.5, min_samples=1)
        labels = db.fit_predict(centers_y)

        cluster_items = {}
        for i, label in enumerate(labels):
            if label not in cluster_items:
                cluster_items[label] = []
            cluster_items[label].append(lines[i])

        sorted_labels = sorted(
            cluster_items.keys(),
            key=lambda lb: np.mean([
                (l['box'].TopLeft.y + l['box'].BottomLeft.y) / 2.0
                for l in cluster_items[lb]
            ])
        )
        groups = [cluster_items[lb] for lb in sorted_labels]
    else:
        # 回退到基于重叠的分组
        sorted_lines = sorted(lines,
                              key=lambda l: (l['box'].TopLeft.y + l['box'].BottomLeft.y) / 2.0)
        groups = [[sorted_lines[0]]]
        for line in sorted_lines[1:]:
            ref_box = groups[-1][0]['box']
            cur_box = line['box']
            min_y1, max_y1 = ref_box.TopLeft.y, ref_box.BottomLeft.y
            min_y2, max_y2 = cur_box.TopLeft.y, cur_box.BottomLeft.y
            intersection = max(0, min(max_y1, max_y2) - max(min_y1, min_y2))
            min_h = min(max_y1 - min_y1, max_y2 - min_y2)
            overlap = intersection / min_h if min_h > 0 else 0

            if overlap > 0.5:
                groups[-1].append(line)
            else:
                groups.append([line])

    if len(groups) < 2:
        return []

    # 行间距分析和合并（重叠组间距钳位到 0，避免负值干扰阈值计算）
    inter_gaps = []
    for k in range(len(groups) - 1):
        bottom = max(l['box'].BottomLeft.y for l in groups[k])
        top = min(l['box'].TopLeft.y for l in groups[k + 1])
        inter_gaps.append(max(0.0, top - bottom))

    merge_threshold = max(0.0, _find_gap_threshold(inter_gaps, avg_h * 0.3))

    merged = [groups[0]]
    for k in range(1, len(groups)):
        if inter_gaps[k - 1] < merge_threshold:
            merged[-1].extend(groups[k])
        else:
            merged.append(groups[k])
    groups = merged

    if len(groups) < 2:
        return []

    boundaries = []
    for k in range(len(groups) - 1):
        bottom = max(l['box'].BottomLeft.y for l in groups[k])
        top = min(l['box'].TopLeft.y for l in groups[k + 1])
        boundaries.append((bottom + top) / 2.0)

    return boundaries


def _build_grid_visual(lines, image_path):
    """策略 1 (混合): 视觉线条 + 文本间距分析共同定行和列"""
    if not lines:
        return None

    h_lines, v_lines = _detect_table_grid(image_path)

    heights = [abs(l['box'].BottomLeft.y - l['box'].TopLeft.y) for l in lines]
    avg_h = max(float(np.mean(heights)), 1.0) if heights else 10.0
    min_dist = avg_h

    # ---- 行边界 ----
    row_bounds = list(h_lines)
    if len(h_lines) < 3:
        # 视觉网格不足，用文本行间距分析补充
        row_splits = _detect_row_splits(lines)
        for y in row_splits:
            if all(abs(y - b) > min_dist for b in row_bounds):
                row_bounds.append(y)
    row_bounds.sort()

    min_y = min(l['box'].TopLeft.y for l in lines) - avg_h
    max_y = max(l['box'].BottomLeft.y for l in lines) + avg_h
    if not row_bounds or row_bounds[0] > min_y + min_dist:
        row_bounds.insert(0, min_y)
    if not row_bounds or row_bounds[-1] < max_y - min_dist:
        row_bounds.append(max_y)

    # 若仍不足 2 个行边界，用文本块包围盒构建最小行范围
    if len(row_bounds) < 2:
        row_bounds = [min_y, max_y]

    # ---- 列边界（使用原始行边界，避免细分后影响列间距分析）----
    gap_splits = _detect_column_splits(lines, row_bounds)
    col_bounds = list(v_lines)
    for x in gap_splits:
        if all(abs(x - b) > min_dist for b in col_bounds):
            col_bounds.append(x)
    col_bounds.sort()

    min_x = min(l['box'].TopLeft.x for l in lines) - avg_h
    max_x = max(l['box'].TopRight.x for l in lines) + avg_h
    if not col_bounds or col_bounds[0] > min_x + min_dist:
        col_bounds.insert(0, min_x)
    if not col_bounds or col_bounds[-1] < max_x - min_dist:
        col_bounds.append(max_x)

    # 若仍不足 2 个列边界，用文本块包围盒构建最小列范围
    if len(col_bounds) < 2:
        col_bounds = [min_x, max_x]

    # ---- 细分包含多行文本的大行带（处理部分无行线的表格）----
    extra_row_bounds = []
    for idx in range(len(row_bounds) - 1):
        band_top = row_bounds[idx]
        band_bottom = row_bounds[idx + 1]

        # 只对高度超过 2 倍文字高度的行带进行细分
        if band_bottom - band_top < avg_h * 2.0:
            continue

        # 收集该行带内的文本行
        band_lines = [l for l in lines
                      if band_top <= (l['box'].TopLeft.y + l['box'].BottomLeft.y) / 2.0 <= band_bottom]
        if len(band_lines) < 2:
            continue

        # 按垂直重叠度聚类为行组（不进行合并，避免将相邻行误合并）
        sorted_bl = sorted(band_lines,
                           key=lambda l: (l['box'].TopLeft.y + l['box'].BottomLeft.y) / 2.0)
        sub_groups = [[sorted_bl[0]]]
        for line in sorted_bl[1:]:
            ref = sub_groups[-1][0]['box']
            cur = line['box']
            inter = max(0.0, min(ref.BottomLeft.y, cur.BottomLeft.y) - max(ref.TopLeft.y, cur.TopLeft.y))
            minh = min(ref.BottomLeft.y - ref.TopLeft.y, cur.BottomLeft.y - cur.TopLeft.y)
            if minh > 0 and inter / minh > 0.5:
                sub_groups[-1].append(line)
            else:
                sub_groups.append([line])

        if len(sub_groups) < 2:
            continue

        # 在相邻组之间添加行边界
        for k in range(len(sub_groups) - 1):
            bot_k = max(l['box'].BottomLeft.y for l in sub_groups[k])
            top_k1 = min(l['box'].TopLeft.y for l in sub_groups[k + 1])
            mid = (bot_k + top_k1) / 2.0
            if (all(abs(mid - b) > avg_h * 0.3 for b in row_bounds) and
                    all(abs(mid - b) > avg_h * 0.3 for b in extra_row_bounds)):
                extra_row_bounds.append(mid)

    if extra_row_bounds:
        row_bounds.extend(extra_row_bounds)
        row_bounds.sort()

    num_rows = len(row_bounds) - 1
    num_cols = len(col_bounds) - 1
    grid = [[""] * num_cols for _ in range(num_rows)]

    for line in lines:
        box = line['box']
        lx = box.TopLeft.x
        cy = (box.TopLeft.y + box.BottomLeft.y) / 2.0

        col = -1
        for j in range(num_cols):
            if col_bounds[j] <= lx < col_bounds[j + 1]:
                col = j
                break
        if col == -1:
            col = min(range(num_cols),
                      key=lambda j: abs(col_bounds[j] - lx))

        row = -1
        for i in range(num_rows):
            if row_bounds[i] <= cy <= row_bounds[i + 1]:
                row = i
                break
        if row == -1:
            row = min(range(num_rows),
                      key=lambda i: abs((row_bounds[i] + row_bounds[i + 1]) / 2.0 - cy))

        if grid[row][col]:
            grid[row][col] += " " + line['text']
        else:
            grid[row][col] = line['text']

    return grid


def _build_grid_clustering(lines):
    """策略 2: 基于行内间距分析构建表格（用于无边框表格的回退）"""
    if not lines:
        return []

    sorted_lines = sorted(lines, key=lambda l: l['box'].TopLeft.y)

    def get_vertical_overlap(box1, box2):
        min_y1, max_y1 = box1.TopLeft.y, box1.BottomLeft.y
        min_y2, max_y2 = box2.TopLeft.y, box2.BottomLeft.y
        intersection = max(0, min(max_y1, max_y2) - max(min_y1, min_y2))
        min_h = min(max_y1 - min_y1, max_y2 - min_y2)
        if min_h <= 0:
            return 0
        return intersection / min_h

    rows = []
    current_row = []
    for line in sorted_lines:
        if not current_row:
            current_row.append(line)
            continue
        if get_vertical_overlap(current_row[-1]['box'], line['box']) > 0.6:
            current_row.append(line)
        else:
            current_row.sort(key=lambda l: l['box'].TopLeft.x)
            rows.append(current_row)
            current_row = [line]
    if current_row:
        current_row.sort(key=lambda l: l['box'].TopLeft.x)
        rows.append(current_row)

    # 收集行内间隙，用于检测列边界
    heights = [abs(l['box'].BottomLeft.y - l['box'].TopLeft.y) for l in lines]
    avg_h = max(float(np.median(heights)), 1.0) if heights else 10.0

    all_gaps = []
    for row_items in rows:
        if len(row_items) < 2:
            continue
        for k in range(len(row_items) - 1):
            right_x = row_items[k]['box'].TopRight.x
            left_x = row_items[k + 1]['box'].TopLeft.x
            gap = left_x - right_x
            if gap > 0:
                all_gaps.append((gap, (right_x + left_x) / 2.0))

    if not all_gaps:
        # 无间隙，所有文本归为单列
        grid = []
        for row_items in rows:
            grid.append([" ".join(item['text'] for item in row_items)])
        return grid

    # 用间隙大小分布区分列内间隙和列间间隙
    gap_sizes = [g for g, _ in all_gaps]
    gap_threshold = _find_gap_threshold(gap_sizes, avg_h * 0.8)
    sig_gap_mids = [mid for size, mid in all_gaps if size > gap_threshold]

    if not sig_gap_mids:
        grid = []
        for row_items in rows:
            grid.append([" ".join(item['text'] for item in row_items)])
        return grid

    # 聚类间隙中点以确定稳定的列边界
    gap_arr = np.array(sig_gap_mids).reshape(-1, 1)
    if len(gap_arr) == 1:
        boundaries = [float(gap_arr[0][0])]
    else:
        min_support = max(2, len(rows) // 5)
        if _HAS_DBSCAN:
            db = _DBSCAN(eps=avg_h * 2, min_samples=min_support)
            labels = db.fit_predict(gap_arr)
            boundaries = []
            for label in set(labels):
                if label == -1:
                    continue
                mask = labels == label
                boundaries.append(float(np.mean(gap_arr[mask])))
        else:
            linkage_mat = hcluster.linkage(gap_arr, method='ward')
            clusters = hcluster.fcluster(linkage_mat, t=avg_h * 2, criterion='distance')
            boundaries = []
            for cid in set(clusters):
                mask = clusters == cid
                if np.sum(mask) >= min_support:
                    boundaries.append(float(np.mean(gap_arr[mask])))

        if not boundaries:
            # min_support 过滤后无结果，放宽到 1
            linkage_mat = hcluster.linkage(gap_arr, method='ward')
            clusters = hcluster.fcluster(linkage_mat, t=avg_h * 2, criterion='distance')
            boundaries = []
            for cid in set(clusters):
                mask = clusters == cid
                boundaries.append(float(np.mean(gap_arr[mask])))

    boundaries.sort()

    # 构建列边界
    min_x = min(l['box'].TopLeft.x for l in lines) - 1
    max_x = max(l['box'].TopRight.x for l in lines) + 1
    col_bounds = [min_x] + boundaries + [max_x]
    num_cols = len(col_bounds) - 1

    # 分配文本到网格
    grid = []
    for row_items in rows:
        row_cells = [""] * num_cols
        for item in row_items:
            lx = item['box'].TopLeft.x
            col = num_cols - 1
            for j in range(num_cols):
                if col_bounds[j] <= lx < col_bounds[j + 1]:
                    col = j
                    break
            if row_cells[col]:
                row_cells[col] += " " + item['text']
            else:
                row_cells[col] = item['text']
        grid.append(row_cells)

    return grid


def reconstruct_table_from_lines(lines, image_path=None):
    """使用文本行的边界框信息重建表格结构，输出为 TSV 格式"""
    if not lines:
        return ""

    # 将边界框坐标四舍五入到整数像素，消除亚像素级别浮动带来的不稳定性
    for line in lines:
        box = line['box']
        for pt_name in ('TopLeft', 'TopRight', 'BottomLeft', 'BottomRight'):
            pt = getattr(box, pt_name)
            pt.x = float(round(pt.x))
            pt.y = float(round(pt.y))

    final_grid = None
    if image_path:
        final_grid = _build_grid_visual(lines, image_path)

    if final_grid is None:
        final_grid = _build_grid_clustering(lines)

    if not final_grid:
        return ""

    # 移除空行
    final_grid = [row for row in final_grid if any(cell.strip() for cell in row)]

    # 移除空列
    if final_grid:
        num_cols = max(len(row) for row in final_grid)
        for row in final_grid:
            while len(row) < num_cols:
                row.append("")
        non_empty = [c for c in range(num_cols) if any(row[c].strip() for row in final_grid)]
        if non_empty:
            final_grid = [[row[c] for c in non_empty] for row in final_grid]

    return "\n".join(["\t".join(r) for r in final_grid])


def use_oneocr_table(image_path):
    """
    使用 OneOCR 进行本地表格识别。
    通过 OCR 获取文本及边界框，再根据位置信息重建表格结构。
    """
    global _oneocr_instance, oneocr_initialized

    _ensure_table_deps()

    if not _HAS_TABLE_DEPS:
        logging.error("表格识别需要 numpy 和 scipy 库")
        return "表格识别不可用：缺少 numpy/scipy 依赖库，请执行 pip install numpy scipy"

    try:
        if _oneocr_instance is None:
            with oneocr_init_lock:
                if _oneocr_instance is None:
                    _oneocr_instance = OneOCR()
                    oneocr_initialized = True
                    logging.info("OneOCR 引擎初始化成功")

        logging.debug(f"开始表格识别图片: {image_path}")

        lines_with_boxes = _oneocr_instance.recognize(image_path, get_boxes=True)

        if isinstance(lines_with_boxes, str):
            logging.error(f"OneOCR 表格识别失败: {lines_with_boxes}")
            return None

        if not lines_with_boxes:
            logging.warning("OneOCR 表格识别返回空结果")
            return None

        result = reconstruct_table_from_lines(lines_with_boxes, image_path=image_path)
        if result:
            logging.debug(f"表格识别成功，结果长度: {len(result)}")
        return result if result else None

    except FileNotFoundError as e:
        logging.error(f"OneOCR 文件未找到: {e}")
        return None
    except RuntimeError as e:
        logging.error(f"OneOCR 运行时错误: {e}")
        return None
    except Exception as e:
        logging.error(f"OneOCR 表格识别失败: {e}")
        return None


# ==================== 腾讯云 OCR API ====================

# 使用公共腾讯云工具模块
from .tencent_utils import tencent_api_request as _tencent_api_request


def use_tencent_ocr(image_path, secret_id, secret_key, api_type='general_basic'):
    """
    腾讯云 OCR 识别
    api_type:
        - general_basic: 通用印刷体识别 (GeneralBasicOCR)
        - general_accurate: 通用文字识别高精度版 (GeneralAccurateOCR)
        - table: 表格识别V3 (RecognizeTableAccurateOCR)
        - formula: 公式识别 (RecognizeFormulaOCR)
    """
    try:
        # 读取图片并转为 Base64
        with open(image_path, 'rb') as f:
            image_base64 = base64.b64encode(f.read()).decode('utf-8')

        # 根据类型选择接口
        action_map = {
            'general_basic': 'GeneralBasicOCR',
            'general_accurate': 'GeneralAccurateOCR',
            'table': 'RecognizeTableAccurateOCR',
            'formula': 'RecognizeFormulaOCR',
        }
        action = action_map.get(api_type, 'GeneralBasicOCR')

        params = {'ImageBase64': image_base64}

        logging.debug(f"Tencent OCR request: action={action}")
        result = _tencent_api_request(secret_id, secret_key, 'ocr', action,
                                      params, 'ocr.tencentcloudapi.com')

        if not result:
            return None

        response = result.get('Response', {})

        # 检查错误
        if 'Error' in response:
            error = response['Error']
            error_code = error.get('Code', 'Unknown')
            error_msg = error.get('Message', 'Unknown error')
            logging.error(f"Tencent OCR API error: {error_code} - {error_msg}")

            # 处理特定错误
            if error_code in ['ResourceUnavailable.InArrears', 'ResourcesSoldOut.ChargeStatusException']:
                return 'LIMIT_REACHED_ERROR'
            if error_code == 'FailedOperation.UnOpenError':
                return 'NO_PERMISSION_ERROR'
            return None

        # 解析结果
        if api_type == 'table':
            # 表格识别返回 Excel 数据，需要解析 TableDetections
            table_detections = response.get('TableDetections', [])
            if not table_detections:
                return None
            text_result = ""
            for table in table_detections:
                cells = table.get('Cells', [])
                if not cells:
                    continue
                # 找出最大行列
                max_row = max(cell.get('RowBr', 0) for cell in cells)
                max_col = max(cell.get('ColBr', 0) for cell in cells)
                # 构建表格
                grid = [['' for _ in range(max_col + 1)] for _ in range(max_row + 1)]
                for cell in cells:
                    row = cell.get('RowTl', 0)
                    col = cell.get('ColTl', 0)
                    text = cell.get('Text', '')
                    grid[row][col] = text.replace('\n', ' ')
                for row in grid:
                    text_result += '\t'.join(row) + '\n'
                text_result += '\n'
            return text_result.strip() if text_result else None

        elif api_type == 'formula':
            # 公式识别
            formula_list = response.get('FormulaInfoList', [])
            if not formula_list:
                return None
            return '\n\n'.join([item.get('DetectedText', '') for item in formula_list])

        else:
            # 通用印刷体/高精度识别
            text_detections = response.get('TextDetections', [])
            if not text_detections:
                return None
            return '\n'.join([item.get('DetectedText', '') for item in text_detections])

    except FileNotFoundError:
        logging.error(f"Image file not found: {image_path}")
        return None
    except Exception as e:
        logging.error(f"Tencent OCR failed: {e}", exc_info=True)
        return None
