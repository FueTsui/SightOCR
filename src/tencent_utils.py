# tencent_utils.py
"""腾讯云 API 公共工具函数"""

import hashlib
import hmac
import json
import time
import logging
import requests
import threading
from datetime import datetime, timezone

# SSL 验证配置（生产环境必须启用）
SSL_VERIFY = True

# HTTP 会话（连接池复用）
_tencent_session = None
_tencent_session_lock = threading.Lock()


def _get_tencent_session():
    """获取或创建腾讯云 HTTP 会话（线程安全，连接池复用）"""
    global _tencent_session
    if _tencent_session is None:
        with _tencent_session_lock:
            if _tencent_session is None:
                _tencent_session = requests.Session()
                adapter = requests.adapters.HTTPAdapter(
                    pool_connections=3,
                    pool_maxsize=5,
                    max_retries=1
                )
                _tencent_session.mount('https://', adapter)
    return _tencent_session


def close_tencent_session():
    """关闭腾讯云 HTTP 会话（程序退出时调用）"""
    global _tencent_session
    with _tencent_session_lock:
        if _tencent_session is not None:
            try:
                _tencent_session.close()
            except Exception:
                pass
            _tencent_session = None


def tencent_sign(secret_key, date, service, string_to_sign):
    """
    腾讯云 TC3-HMAC-SHA256 签名算法

    Args:
        secret_key: 密钥
        date: 日期字符串 (YYYY-MM-DD)
        service: 服务名称 (ocr, tmt 等)
        string_to_sign: 待签名字符串

    Returns:
        签名结果（十六进制字符串）
    """
    def _hmac_sha256(key, msg):
        return hmac.new(key, msg.encode('utf-8'), hashlib.sha256).digest()

    secret_date = _hmac_sha256(('TC3' + secret_key).encode('utf-8'), date)
    secret_service = _hmac_sha256(secret_date, service)
    secret_signing = _hmac_sha256(secret_service, 'tc3_request')
    return hmac.new(secret_signing, string_to_sign.encode('utf-8'), hashlib.sha256).hexdigest()


def tencent_api_request(secret_id, secret_key, service, action, params, host, region=None, version=None):
    """
    腾讯云 API 通用请求函数

    Args:
        secret_id: SecretId
        secret_key: SecretKey
        service: 服务名称 (ocr, tmt 等)
        action: API 操作名称
        params: 请求参数字典
        host: API 主机地址
        region: 地域（可选）
        version: API 版本（可选，默认根据服务自动选择）

    Returns:
        API 响应 JSON，失败返回 None
    """
    endpoint = f'https://{host}'
    algorithm = 'TC3-HMAC-SHA256'
    timestamp = int(time.time())
    date = datetime.fromtimestamp(timestamp, tz=timezone.utc).strftime('%Y-%m-%d')

    # 默认版本号
    if version is None:
        version = '2018-11-19' if service == 'ocr' else '2018-03-21'

    # 请求体
    payload = json.dumps(params)

    # 拼接规范请求串
    http_request_method = 'POST'
    canonical_uri = '/'
    canonical_querystring = ''
    ct = 'application/json; charset=utf-8'
    canonical_headers = f'content-type:{ct}\nhost:{host}\nx-tc-action:{action.lower()}\n'
    signed_headers = 'content-type;host;x-tc-action'
    hashed_request_payload = hashlib.sha256(payload.encode('utf-8')).hexdigest()
    canonical_request = (f'{http_request_method}\n{canonical_uri}\n{canonical_querystring}\n'
                         f'{canonical_headers}\n{signed_headers}\n{hashed_request_payload}')

    # 拼接待签名字符串
    credential_scope = f'{date}/{service}/tc3_request'
    hashed_canonical_request = hashlib.sha256(canonical_request.encode('utf-8')).hexdigest()
    string_to_sign = f'{algorithm}\n{timestamp}\n{credential_scope}\n{hashed_canonical_request}'

    # 计算签名
    signature = tencent_sign(secret_key, date, service, string_to_sign)

    # 拼接 Authorization
    authorization = (f'{algorithm} Credential={secret_id}/{credential_scope}, '
                     f'SignedHeaders={signed_headers}, Signature={signature}')

    # 请求头
    headers = {
        'Authorization': authorization,
        'Content-Type': ct,
        'Host': host,
        'X-TC-Action': action,
        'X-TC-Timestamp': str(timestamp),
        'X-TC-Version': version,
    }
    if region:
        headers['X-TC-Region'] = region

    try:
        session = _get_tencent_session()
        response = session.post(endpoint, headers=headers, data=payload, timeout=30,
                                verify=SSL_VERIFY, proxies={'http': None, 'https': None})
        response.raise_for_status()
        return response.json()
    except requests.exceptions.Timeout:
        logging.error(f"Tencent API request timed out: {action}")
        return None
    except requests.exceptions.RequestException as e:
        logging.error(f"Tencent API request failed: {e}")
        return None
    except Exception as e:
        logging.error(f"Tencent API unexpected error: {e}", exc_info=True)
        return None
