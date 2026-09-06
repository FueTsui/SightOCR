import socket
import requests
from requests.adapters import HTTPAdapter
import re
import json
import time
import random
import threading
import atexit
from hashlib import md5
import logging

# SSL 验证配置（生产环境必须启用）
SSL_VERIFY = True

# Edge 翻译会话（线程安全的单例模式）
_edge_session = None
_edge_session_lock = threading.Lock()

# Edge 翻译 Token 缓存
_edge_token_lock = threading.Lock()
_edge_token_fetch_lock = threading.Lock()  # 防止并发重复获取 token
EDGE_TOKEN_CACHE = {
    "token": "",
    "timestamp": 0
}
EDGE_CACHE_DURATION = 60 * 8  # Edge token 有效期约10分钟，8分钟刷新

# 保活相关常量：
# 长时间空闲后，持久连接会被对端/NAT 静默断开、token 也会过期，导致空闲后第一次
# 翻译需要重新 DNS+TCP+TLS 握手（甚至命中半开 socket 干等到读超时）。保活线程在
# 默认翻译启用时周期性地刷新 token 并保持翻译主机连接处于“热”状态。
EDGE_KEEPALIVE_INTERVAL = 90      # 心跳间隔（秒），需 < 服务器空闲断连窗口（约 120s）
EDGE_TOKEN_REFRESH_AHEAD = 60 * 6  # token 超过该年龄即主动刷新，确保刷新发生在后台而非用户请求路径

# 拆分 connect/read 超时，并适度收紧 read，避免死连接长时间干等
EDGE_AUTH_TIMEOUT = (5, 8)        # 取 token（小请求）
EDGE_TRANSLATE_TIMEOUT = (5, 12)  # 翻译请求

# 启用 TCP keepalive 的 socket 选项，配合保活心跳降低命中半开死连接的概率
_KEEPALIVE_SOCKET_OPTIONS = [
    (socket.IPPROTO_TCP, socket.TCP_NODELAY, 1),
    (socket.SOL_SOCKET, socket.SO_KEEPALIVE, 1),
]


class _KeepAliveHTTPAdapter(HTTPAdapter):
    """为底层连接启用 TCP keepalive 的 HTTPAdapter。"""

    def init_poolmanager(self, *args, **kwargs):
        kwargs.setdefault('socket_options', _KEEPALIVE_SOCKET_OPTIONS)
        super().init_poolmanager(*args, **kwargs)


def get_edge_session() -> requests.Session:
    """获取或创建 Edge 翻译会话（线程安全）"""
    global _edge_session
    if _edge_session is None:
        with _edge_session_lock:
            if _edge_session is None:
                _edge_session = requests.Session()
                _edge_session.headers.update({
                    'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0',
                    'Accept': '*/*',
                    'Accept-Language': 'zh-CN,zh;q=0.9,en;q=0.8',
                })
                # 启用 keepalive，并允许对 GET（取 token）做一次连接级重试，
                # 使空闲后命中的死连接能快速换新连接重发，而非把超时直接抛给用户。
                adapter = _KeepAliveHTTPAdapter(
                    pool_connections=4,
                    pool_maxsize=8,
                    max_retries=1
                )
                _edge_session.mount('https://', adapter)
                _edge_session.mount('http://', adapter)
                logging.debug("Edge translation session created")
    return _edge_session


def close_edge_session():
    """关闭 Edge 翻译会话"""
    global _edge_session
    with _edge_session_lock:
        if _edge_session is not None:
            try:
                _edge_session.close()
                logging.debug("Edge session closed successfully")
            except Exception as e:
                logging.warning(f"关闭 Edge 会话时出错: {e}")
            finally:
                _edge_session = None


def get_edge_token(force_new=False):
    """获取 Edge 翻译 JWT Token（并发安全，自动去重）"""
    global EDGE_TOKEN_CACHE

    # 快速路径：缓存有效时直接返回，无需竞争 fetch lock
    if not force_new:
        with _edge_token_lock:
            if EDGE_TOKEN_CACHE["token"] and (time.time() - EDGE_TOKEN_CACHE["timestamp"] < EDGE_CACHE_DURATION):
                logging.debug("Using cached Edge token.")
                return EDGE_TOKEN_CACHE["token"]

    # 串行化实际的网络请求，避免并发重复获取
    with _edge_token_fetch_lock:
        # 获得锁后再次检查缓存（另一个线程可能刚刚完成获取）
        if not force_new:
            with _edge_token_lock:
                if EDGE_TOKEN_CACHE["token"] and (time.time() - EDGE_TOKEN_CACHE["timestamp"] < EDGE_CACHE_DURATION):
                    logging.debug("Using cached Edge token (after fetch lock).")
                    return EDGE_TOKEN_CACHE["token"]

        logging.debug("Fetching new Edge translation token.")
        auth_url = 'https://edge.microsoft.com/translate/auth'

        try:
            session = get_edge_session()
            response = session.get(auth_url, timeout=EDGE_AUTH_TIMEOUT, proxies={'http': None, 'https': None})
            response.raise_for_status()

            token = response.text.strip()
            if token:
                with _edge_token_lock:
                    EDGE_TOKEN_CACHE["token"] = token
                    EDGE_TOKEN_CACHE["timestamp"] = time.time()
                logging.debug("New Edge token cached.")
                return token

            logging.error("Edge token response is empty")
            return None

        except Exception as e:
            logging.error(f"Failed to get Edge token: {e}")
            return None


def clear_sensitive_cache():
    """清理敏感的令牌缓存（程序退出时调用）"""
    global EDGE_TOKEN_CACHE
    with _edge_token_lock:
        EDGE_TOKEN_CACHE["token"] = ""
        EDGE_TOKEN_CACHE["timestamp"] = 0
    close_edge_session()
    logging.debug("翻译令牌缓存已清理")


# 注册 atexit 钩子，确保程序退出时清理资源
atexit.register(clear_sensitive_cache)


# Edge 翻译语言代码映射
# 界面内部语言码即为 Edge（BCP-47）代码，可直接透传，此处仅处理特殊值：
# - auto: Edge API 使用空字符串表示自动检测
# - zh:   旧版内部代码（保活请求等仍在使用）
# - nn:   Edge API 不支持新挪威语，回退为书面挪威语（百度接口支持真正的新挪威语）
EDGE_LANG_MAP = {
    'auto': '',
    'zh': 'zh-Hans',
    'nn': 'nb',
}


def bing_trans(word, from_lang='auto', to_lang='zh', max_retries=3):
    """
    必应翻译（使用 Edge 浏览器翻译 API）
    Edge 翻译 API 比网页版更稳定，不易触发 429 限制
    """
    try:
        logging.debug(f"Starting Edge translation for: '{word[:50]}...', from: {from_lang}, to: {to_lang}")

        # 转换语言代码
        source_lang = EDGE_LANG_MAP.get(from_lang, from_lang)
        target_lang = EDGE_LANG_MAP.get(to_lang, to_lang)

        # 如果源语言和目标语言相同，自动调整
        if source_lang and source_lang == target_lang:
            target_lang = 'en' if source_lang == 'zh-Hans' else 'zh-Hans'

        for attempt in range(max_retries):
            try:
                token = get_edge_token()
                if not token:
                    logging.warning(f"Failed to get Edge token. Attempt {attempt+1}/{max_retries}")
                    time.sleep(2 + random.random())
                    continue

                # Edge 翻译 API 端点
                translate_url = 'https://api-edge.cognitive.microsofttranslator.com/translate'

                params = {
                    'api-version': '3.0',
                    'to': target_lang,
                }
                # 如果指定了源语言（非自动检测），添加 from 参数
                if source_lang:
                    params['from'] = source_lang

                headers = {
                    'Authorization': f'Bearer {token}',
                    'Content-Type': 'application/json',
                    'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0',
                }

                # 请求体
                body = [{'Text': word}]

                session = get_edge_session()
                response = session.post(
                    translate_url,
                    params=params,
                    headers=headers,
                    json=body,
                    timeout=EDGE_TRANSLATE_TIMEOUT,
                    proxies={'http': None, 'https': None}
                )

                if response.status_code == 401:
                    # Token 过期，刷新
                    logging.warning(f"Edge token expired. Attempt {attempt+1}/{max_retries}. Refreshing...")
                    get_edge_token(force_new=True)
                    time.sleep(0.5 + random.random())
                    continue

                if response.status_code == 429:
                    wait_time = 3 + attempt * 2 + random.random()
                    logging.warning(f"Edge rate limit (429). Attempt {attempt+1}/{max_retries}. Waiting {wait_time:.1f}s...")
                    time.sleep(wait_time)
                    get_edge_token(force_new=True)
                    continue

                response.raise_for_status()
                result = response.json()

                if result and len(result) > 0:
                    translations = result[0].get('translations', [])
                    if translations:
                        translated_text = translations[0].get('text', '')
                        if translated_text:
                            logging.debug(f"Edge translated: {translated_text[:50]}...")
                            return translated_text

                logging.warning(f"Edge response missing translations: {result}")

            except requests.exceptions.Timeout:
                logging.warning(f"Edge translation timed out on attempt {attempt+1}")
                time.sleep(2 + random.random())
            except requests.exceptions.RequestException as e:
                logging.warning(f"Edge translation request failed on attempt {attempt+1}: {e}")
                time.sleep(2 + random.random())
            except (KeyError, IndexError, json.JSONDecodeError) as e:
                logging.warning(f"Edge response parsing failed on attempt {attempt+1}: {e}")
                time.sleep(1 + random.random())

        logging.error("Edge 翻译多次重试后仍失败。")
        return None

    except Exception as e:
        logging.error(f"Edge 翻译未知异常: {e}", exc_info=True)
        return None


def keep_edge_warm():
    """周期性保活：在默认翻译启用时由 TranslationManager 调用。

    1) 在 token 自然过期（8 分钟）之前主动刷新，确保刷新发生在后台保活线程，
       而不是用户按热键后的请求路径上；
    2) 发起一次极小翻译，使真正的翻译主机
       (api-edge.cognitive.microsofttranslator.com) 的 TCP/TLS 连接保持“热”，
       避免空闲后首次翻译的冷重连延迟。

    返回 True 表示保活成功。所有异常都被吞掉（仅记录 debug），不影响主流程。
    """
    try:
        with _edge_token_lock:
            token_age = time.time() - EDGE_TOKEN_CACHE["timestamp"]
            has_token = bool(EDGE_TOKEN_CACHE["token"])

        # 提前刷新即将过期或缺失的 token（强制走网络，顺带保持取 token 主机连接热）
        if (not has_token) or token_age > EDGE_TOKEN_REFRESH_AHEAD:
            if not get_edge_token(force_new=True):
                logging.debug("Edge keepalive: token refresh failed")
                return False

        # 极小翻译，仅用于保持翻译主机连接，结果丢弃
        bing_trans("a", from_lang="en", to_lang="zh", max_retries=1)
        return True
    except Exception as e:
        logging.debug(f"Edge keepalive failed: {e}")
        return False


# Baidu translation (unchanged, for fallback if needed)
def make_md5(s, encoding='utf-8'):
    return md5(s.encode(encoding)).hexdigest()

# 百度翻译语言代码映射（内部码 → 百度语种码）
# 与内部码一致的语种（en/de/ru/it/tr/th/pl/nl/id/hi 等）无需列出；
# 百度不支持的语种（蒙古语、乌兹别克语等）原样透传，接口报错返回 None 后
# 由 TranslationManager 自动回退到默认翻译
BAIDU_LANG_MAP = {
    'zh-Hans': 'zh',
    'zh-Hant': 'cht',
    'ja': 'jp',
    'ko': 'kor',
    'fr': 'fra',
    'es': 'spa',
    'pt-PT': 'pt',
    'pt': 'pt',
    'vi': 'vie',
    'ms': 'may',
    'ar': 'ara',
    'km': 'hkm',
    'nb': 'nob',
    'nn': 'nno',
    'fa': 'per',
    'sv': 'swe',
    'uk': 'ukr',
}

def baidu_trans(query, appid, appkey, from_lang='auto', to_lang='zh'):
    """百度翻译 API"""
    endpoint = 'https://api.fanyi.baidu.com'
    path = '/api/trans/vip/translate'
    url = endpoint + path

    from_lang = BAIDU_LANG_MAP.get(from_lang, from_lang)
    to_lang = BAIDU_LANG_MAP.get(to_lang, to_lang)

    if from_lang != 'auto' and from_lang == to_lang:
        to_lang = 'en' if from_lang == 'zh' else 'zh'

    salt = random.randint(32768, 65536)
    sign = make_md5(appid + query + str(salt) + appkey)

    headers = {'Content-Type': 'application/x-www-form-urlencoded'}
    payload = {
        'appid': appid,
        'q': query,
        'from': from_lang,
        'to': to_lang,
        'salt': salt,
        'sign': sign
    }

    try:
        logging.debug(f"Attempting Baidu translation")
        r = requests.post(url, params=payload, headers=headers, timeout=10,
                          proxies={'http': None, 'https': None})
        r.raise_for_status()
        result = r.json()
        logging.debug(f"Baidu response received")

        # 统一将 error_code 转为字符串比较
        error_code = str(result.get('error_code', ''))

        if error_code == '52001':
            logging.warning("Baidu: Source and target lang are the same. Retrying with fallback.")
            detected_from = to_lang
            new_to = 'en' if detected_from == 'zh' else 'zh'

            payload['from'] = detected_from
            payload['to'] = new_to
            salt = random.randint(32768, 65536)
            sign = make_md5(appid + query + str(salt) + appkey)
            payload['salt'] = salt
            payload['sign'] = sign

            logging.debug(f"Retrying Baidu translation")
            r = requests.post(url, params=payload, headers=headers, timeout=10,
                              proxies={'http': None, 'https': None})
            r.raise_for_status()
            result = r.json()
            error_code = str(result.get('error_code', ''))

        if error_code:
            error_msg = result.get('error_msg', '未知错误')
            logging.error(f"Baidu Translate API error. Code: {error_code}, Message: {error_msg}")
            return None

        trans_result = result.get("trans_result", [])
        if not trans_result:
            logging.error(f"Baidu response missing 'trans_result': {result}")
            return None

        translated_text = trans_result[0].get('dst', '')
        return translated_text if translated_text else None

    except requests.exceptions.Timeout:
        logging.error("Baidu translate request timed out")
        return None
    except Exception as e:
        logging.error(f"Exception in baidu_trans: {e}", exc_info=True)
        return None


# ==================== 腾讯翻译 API ====================

# 使用公共腾讯云工具模块
from .tencent_utils import tencent_api_request

# 腾讯翻译语言代码映射（内部码 → 腾讯语种码）
# 文本翻译仅支持下列语种；未列出的语种（粤语、蒙古语、高棉语、挪威语、
# 波斯语、瑞典语、波兰语、荷兰语、乌克兰语、乌兹别克语等）原样透传，
# 接口报错返回 None 后由 TranslationManager 自动回退到默认翻译
TENCENT_LANG_MAP = {
    'auto': 'auto',
    'zh': 'zh',
    'zh-Hans': 'zh',
    'zh-Hant': 'zh-TW',
    'en': 'en',
    'ja': 'ja',
    'ko': 'ko',
    'fr': 'fr',
    'es': 'es',
    'it': 'it',
    'de': 'de',
    'tr': 'tr',
    'ru': 'ru',
    'pt': 'pt',
    'pt-PT': 'pt',
    'vi': 'vi',
    'id': 'id',
    'th': 'th',
    'ms': 'ms',
    'ar': 'ar',
    'hi': 'hi',
}


def tencent_trans(query, secret_id, secret_key, from_lang='auto', to_lang='zh'):
    """腾讯云翻译 API"""
    try:
        # 转换语言代码
        source = TENCENT_LANG_MAP.get(from_lang, from_lang)
        target = TENCENT_LANG_MAP.get(to_lang, to_lang)

        # 如果源语言和目标语言相同，自动调整
        if source != 'auto' and source == target:
            target = 'en' if source == 'zh' else 'zh'

        # 请求参数
        params = {
            'SourceText': query,
            'Source': source,
            'Target': target,
            'ProjectId': 0
        }

        logging.debug(f"Tencent translate request: source={source}, target={target}")

        # 使用公共 API 请求函数
        result = tencent_api_request(
            secret_id=secret_id,
            secret_key=secret_key,
            service='tmt',
            action='TextTranslate',
            params=params,
            host='tmt.tencentcloudapi.com',
            region='ap-guangzhou',
            version='2018-03-21'
        )

        if not result:
            return None

        resp = result.get('Response', {})

        # 检查错误
        if 'Error' in resp:
            error = resp['Error']
            error_code = error.get('Code', 'Unknown')
            error_msg = error.get('Message', 'Unknown error')
            logging.error(f"Tencent Translate API error: {error_code} - {error_msg}")
            return None

        translated_text = resp.get('TargetText', '')
        if translated_text:
            logging.debug(f"Tencent translated: {translated_text[:50]}...")
            return translated_text

        logging.error(f"Tencent translate response missing 'TargetText': {result}")
        return None

    except Exception as e:
        logging.error(f"Tencent translate failed: {e}", exc_info=True)
        return None