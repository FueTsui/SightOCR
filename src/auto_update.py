# auto_update.py

import datetime
import re
import requests
import tempfile
import os
import sys
import subprocess
import logging
import hashlib

from .paths import get_root_dir

# 版本号的唯一可信来源（single source of truth）：resource.rc。
# 仅当自动探测失败时才回退到此默认值，避免版本号在多处手工维护导致不一致。
_FALLBACK_VERSION = "1.8.4"


def _read_version_from_rc():
    """开发环境：从项目根目录的 resource.rc 解析版本号。

    优先解析结构化的 filevers=(1, 8, 4, 0) 元组；失败再尝试 ProductVersion 字符串。
    返回形如 "1.8.4" 的字符串，解析失败返回 None。
    """
    rc_path = os.path.join(get_root_dir(), "resource.rc")
    try:
        with open(rc_path, "r", encoding="utf-8") as f:
            content = f.read()
    except Exception:
        return None

    # filevers=(1, 8, 4, 0) -> 取前三段
    m = re.search(r"filevers\s*=\s*\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)", content)
    if m:
        return f"{m.group(1)}.{m.group(2)}.{m.group(3)}"

    # 回退：ProductVersion u'1.8.4'
    m = re.search(r"ProductVersion['\"]?\s*,\s*u?['\"]([\d.]+)['\"]", content)
    if m:
        return m.group(1).rstrip(".")

    return None


def _read_version_from_exe():
    """打包环境：从可执行文件自身嵌入的版本资源读取版本号。

    PyInstaller 把 resource.rc 编译进 exe 的版本资源（而非随包附带 .rc 文件），
    因此打包后无法读源文件，改用 Win32 版本信息 API 读取同一份数据。
    """
    try:
        import win32api
        info = win32api.GetFileVersionInfo(sys.executable, "\\")
        ms = info["FileVersionMS"]
        ls = info["FileVersionLS"]
        return f"{ms >> 16}.{ms & 0xFFFF}.{ls >> 16}"
    except Exception:
        return None


def _detect_current_version():
    """探测当前版本号：打包态读 exe 版本资源，开发态读 resource.rc，均失败回退默认值。"""
    version = None
    if getattr(sys, "frozen", False):
        version = _read_version_from_exe()
    else:
        version = _read_version_from_rc()

    if not version:
        logging.warning(f"无法自动探测版本号，回退到默认值 {_FALLBACK_VERSION}")
        version = _FALLBACK_VERSION
    return version


CURRENT_VERSION = _detect_current_version()

# 更新下载配置
GITHUB_API_URL = "https://api.github.com/repos/FueTsui/SightOCR/releases/latest"
MIN_INSTALLER_SIZE = 10 * 1024 * 1024  # 最小安装包大小 10MB


def version_tuple(v: str) -> tuple:
    """将版本字符串转换为可比较的元组"""
    try:
        return tuple(int(part) for part in v.lstrip('v').split('.') if part.isdigit())
    except (ValueError, AttributeError):
        return (0, 0, 0)


def calculate_file_hash(file_path, algorithm='sha256'):
    """计算文件哈希值"""
    hash_obj = hashlib.new(algorithm)
    try:
        with open(file_path, 'rb') as f:
            for chunk in iter(lambda: f.read(8192), b''):
                hash_obj.update(chunk)
        return hash_obj.hexdigest()
    except Exception as e:
        logging.error(f"计算文件哈希失败: {e}")
        return None


def get_release_info():
    """从 GitHub 获取最新版本信息"""
    try:
        headers = {
            'Accept': 'application/vnd.github.v3+json',
            'User-Agent': 'SightOCR-Updater'
        }
        resp = requests.get(GITHUB_API_URL, headers=headers, timeout=15)
        resp.raise_for_status()
        data = resp.json()

        latest_tag = data.get("tag_name", "").lstrip("v")
        latest_version = latest_tag.split("-")[0]

        # 尝试从 release body 中获取 checksum（格式：SHA256: xxxx）
        body = data.get("body", "")
        expected_hash = None
        for line in body.split('\n'):
            if line.strip().upper().startswith("SHA256:"):
                expected_hash = line.split(":", 1)[1].strip().lower()
                break

        # 从 assets 中获取安装包下载链接
        download_url = None
        assets = data.get("assets", [])
        for asset in assets:
            asset_name = asset.get("name", "").lower()
            # 查找 .exe 安装包
            if asset_name.endswith(".exe") and "setup" in asset_name:
                download_url = asset.get("browser_download_url")
                break

        # 如果没找到 setup.exe，尝试找任意 .exe 文件
        if not download_url:
            for asset in assets:
                if asset.get("name", "").lower().endswith(".exe"):
                    download_url = asset.get("browser_download_url")
                    break

        return {
            "version": latest_version,
            "tag": latest_tag,
            "expected_hash": expected_hash,
            "release_url": data.get("html_url", ""),
            "download_url": download_url
        }
    except requests.exceptions.Timeout:
        logging.error("获取版本信息超时")
        return None
    except Exception as e:
        logging.error(f"获取版本信息失败: {e}")
        return None


def download_update(download_url, dest_path):
    """下载更新文件"""
    try:
        logging.info(f"下载更新包: {download_url}")
        with requests.get(download_url, stream=True, timeout=300) as r:
            r.raise_for_status()
            total_size = int(r.headers.get('content-length', 0))

            with open(dest_path, "wb") as f:
                downloaded = 0
                for chunk in r.iter_content(chunk_size=8192):
                    if chunk:
                        f.write(chunk)
                        downloaded += len(chunk)

            # 验证下载完整性
            if total_size > 0 and downloaded != total_size:
                logging.error(f"下载不完整: 期望 {total_size} 字节，实际 {downloaded} 字节")
                return False

            if downloaded < MIN_INSTALLER_SIZE:
                logging.error(f"下载文件过小: {downloaded} 字节")
                return False

            logging.info(f"下载完成: {downloaded} 字节")
            return True

    except requests.exceptions.Timeout:
        logging.error("下载超时")
        return False
    except Exception as e:
        logging.error(f"下载失败: {e}")
        return False


def verify_installer(file_path, expected_hash=None):
    """验证安装包完整性"""
    if not os.path.exists(file_path):
        logging.error("安装包文件不存在")
        return False

    file_size = os.path.getsize(file_path)
    if file_size < MIN_INSTALLER_SIZE:
        logging.error(f"安装包文件过小: {file_size} 字节")
        return False

    # 如果提供了预期哈希值，进行校验
    if expected_hash:
        actual_hash = calculate_file_hash(file_path)
        if actual_hash and actual_hash.lower() != expected_hash.lower():
            logging.error(f"安装包哈希校验失败: 期望 {expected_hash}, 实际 {actual_hash}")
            return False
        elif actual_hash:
            logging.info("安装包哈希校验通过")

    return True


def check_for_update():
    """检查并执行更新"""
    try:
        # 获取最新版本信息
        release_info = get_release_info()
        if not release_info:
            return False

        latest_version = release_info["version"]
        download_url = release_info.get("download_url")

        if version_tuple(latest_version) <= version_tuple(CURRENT_VERSION):
            logging.info(f"当前版本 {CURRENT_VERSION} 已为最新")
            return False

        if not download_url:
            logging.error("未找到安装包下载链接")
            return False

        logging.info(f"发现新版本 {latest_version}，开始自动更新...")

        # 下载更新
        temp_dir = tempfile.gettempdir()
        installer_path = os.path.join(temp_dir, f"SightOCR_{latest_version}_Setup.exe")

        # 如果文件已存在，先删除
        if os.path.exists(installer_path):
            try:
                os.remove(installer_path)
            except Exception:
                pass

        if not download_update(download_url, installer_path):
            return False

        # 验证安装包
        if not verify_installer(installer_path, release_info.get("expected_hash")):
            # 删除可能损坏的文件
            try:
                os.remove(installer_path)
            except Exception:
                pass
            return False

        logging.info("启动静默安装...")

        # 启动安装程序
        try:
            subprocess.Popen(
                [
                    installer_path,
                    "/VERYSILENT",
                    "/SUPPRESSMSGBOXES",
                    "/CLOSEAPPLICATIONS",
                    "/FORCECLOSEAPPLICATIONS"
                ],
                creationflags=subprocess.CREATE_NO_WINDOW if hasattr(subprocess, 'CREATE_NO_WINDOW') else 0
            )
            return True  # 开始更新
        except Exception as e:
            logging.error(f"启动安装程序失败: {e}")
            return False

    except Exception as e:
        logging.error(f"自动更新失败: {e}", exc_info=True)
        return False


def perform_daily_update_check():
    """执行每日更新检查"""
    try:
        # 通过 ConfigManager 单例读写，保证与主程序的内存配置一致，
        # 避免直接写盘后被 ConfigManager.save() 用旧内存副本覆盖。
        from .managers.config_manager import ConfigManager
        config_mgr = ConfigManager.instance()

        today = datetime.date.today().isoformat()
        last_check = config_mgr.get("last_update_check")

        if last_check != today:
            is_updating = check_for_update()
            config_mgr.set("last_update_check", today, auto_save=True)
            return is_updating
        return False
    except Exception as e:
        logging.error(f"每日更新检查失败: {e}")
        return False
