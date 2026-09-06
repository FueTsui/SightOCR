# config.py

import json
import threading
import os
import sys
import time
import logging
import tempfile
import stat
from .paths import get_config_file, get_root_dir

ROOT_DIR = get_root_dir()
CONFIG_FILE = get_config_file()
config_lock = threading.Lock()

# 默认配置
DEFAULT_CONFIG = {
    "hide_tray_icon": False,
    "replace_newline": False,
    "hotkey": "F4",
    "translate_hotkey": "F2",
    "last_ocr_selection": "默认",
    "last_translate_selection": "默认"
}


def load_config():
    """
    加载配置文件，最多重试3次。
    返回元组 (配置字典, 是否使用默认配置)。
    为了向后兼容，也可以只使用返回值作为配置字典。
    """
    for attempt in range(3):
        with config_lock:
            if not os.path.exists(CONFIG_FILE):
                logging.info(f"配置文件不存在，使用默认设置: {CONFIG_FILE}")
                result = DEFAULT_CONFIG.copy()
                result['_config_load_failed'] = False  # 文件不存在不算失败
                return result

            try:
                with open(CONFIG_FILE, 'r', encoding='utf-8') as f:
                    config_data = json.load(f)
                logging.debug("配置文件加载成功")
                # 合并默认配置，确保新增的配置项有默认值
                merged_config = DEFAULT_CONFIG.copy()
                merged_config.update(config_data)
                merged_config['_config_load_failed'] = False
                return merged_config
            except json.JSONDecodeError as e:
                logging.warning(f"第 {attempt + 1} 次加载配置失败（JSON解析错误）: {e}")
            except PermissionError as e:
                logging.warning(f"第 {attempt + 1} 次加载配置失败（权限错误）: {e}")
            except Exception as e:
                logging.warning(f"第 {attempt + 1} 次加载配置失败: {e}")

        # 仅在失败时重试并等待
        if attempt < 2:
            time.sleep(0.5)

    # 所有尝试均失败
    logging.error("多次尝试后仍无法加载配置文件，使用默认设置")
    result = DEFAULT_CONFIG.copy()
    result['_config_load_failed'] = True  # 标记加载失败
    return result


def save_config(config_data):
    """
    原子性保存配置文件。
    使用临时文件写入后替换，确保即使写入中断也不会损坏原文件。
    """
    with config_lock:
        try:
            # 移除内部哨兵字段，避免被写入磁盘
            config_data = {k: v for k, v in config_data.items()
                           if not k.startswith('_')}

            # 确保目录存在
            config_dir = os.path.dirname(CONFIG_FILE)
            if config_dir:
                os.makedirs(config_dir, exist_ok=True)

            # 创建临时文件（与目标文件同目录，确保在同一文件系统）
            fd, temp_path = tempfile.mkstemp(
                suffix='.tmp',
                prefix='config_',
                dir=config_dir if config_dir else '.'
            )

            try:
                # 写入临时文件
                with os.fdopen(fd, 'w', encoding='utf-8') as f:
                    json.dump(config_data, f, ensure_ascii=False, indent=4)

                # os.replace 是原子操作：成功则替换，失败则原文件不受影响
                os.replace(temp_path, CONFIG_FILE)

                # 设置配置文件权限（仅所有者可读写）
                try:
                    os.chmod(CONFIG_FILE, stat.S_IRUSR | stat.S_IWUSR)
                except Exception as perm_err:
                    logging.warning(f"无法设置配置文件权限: {perm_err}")

                logging.debug("配置文件保存成功")
                return True

            except Exception as e:
                # 清理临时文件
                try:
                    if os.path.exists(temp_path):
                        os.remove(temp_path)
                        logging.debug(f"已清理临时配置文件: {temp_path}")
                except Exception as cleanup_err:
                    logging.warning(f"清理临时配置文件失败: {cleanup_err}")
                raise e

        except PermissionError as e:
            logging.error(f"保存配置文件失败（权限错误）: {e}")
            return False
        except Exception as e:
            logging.error(f"保存配置文件失败: {e}")
            return False
