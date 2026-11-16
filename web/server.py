#!/usr/bin/env python3
"""婴儿弹道计算机 (Baby Ballistic Computer) — Web Server, Auth, Invite Codes & 30-Slot Cloud Saves.

Zero third-party dependencies (Python 3.10+ standard library only: http.server, sqlite3, hashlib.scrypt).
Designed to bind strictly to 127.0.0.1 behind a dedicated Cloudflare Tunnel.
"""

from __future__ import annotations

import argparse
import datetime as dt
import gzip
import hashlib
import hmac
import http.cookies
import json
import mimetypes
import os
import re
import secrets
import shutil
import sqlite3
import threading
import time
import urllib.parse
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

MAX_PRESETS_PER_USER = 30
MAX_PRESET_JSON_BYTES = 24 * 1024
MAX_BODY_BYTES = 64 * 1024
SESSION_TTL_SECONDS = 30 * 86400
COOKIE_NAME = "bb_session"
INVITE_ALPHABET = "23456789ABCDEFGHJKLMNPQRSTUVWXYZ"
USERNAME_RE = re.compile(r"^[\w\u4e00-\u9fff\-\.]{2,32}$")

MIME_OVERRIDES = {
    ".js": "application/javascript; charset=utf-8",
    ".html": "text/html; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".json": "application/json; charset=utf-8",
    ".webmanifest": "application/manifest+json; charset=utf-8",
    ".svg": "image/svg+xml",
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".png": "image/png",
    ".bin": "application/octet-stream",
    ".hpred": "application/octet-stream",
}

APP_ICON_SVG = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128">
  <defs>
    <radialGradient id="g" cx="35%" cy="30%" r="70%">
      <stop offset="0%" stop-color="#ffd073"/>
      <stop offset="60%" stop-color="#f0a830"/>
      <stop offset="100%" stop-color="#9a5e10"/>
    </radialGradient>
  </defs>
  <rect width="128" height="128" rx="28" fill="#12171c"/>
  <rect x="10" y="10" width="108" height="108" rx="22" fill="url(#g)"/>
  <text x="64" y="84" text-anchor="middle" font-family="Microsoft YaHei UI,PingFang SC,sans-serif" font-weight="800" font-size="64" fill="#231502">弹</text>
</svg>""".encode("utf-8")

APP_MANIFEST_JSON = json.dumps(
    {
        "name": "婴儿弹道计算机",
        "short_name": "弹道计算机",
        "description": "战争雷霆陆战精细地图、战区边界与三维弹道解算",
        "start_url": "/",
        "display": "standalone",
        "background_color": "#0d1013",
        "theme_color": "#0d1013",
        "icons": [
            {"src": "/icon.svg", "sizes": "any", "type": "image/svg+xml", "purpose": "any maskable"}
        ],
    },
    ensure_ascii=False,
).encode("utf-8")


def now_iso() -> str:
    return dt.datetime.now(dt.timezone.utc).astimezone().isoformat(timespec="seconds")


def hash_password(password: str, salt_hex: str | None = None) -> tuple[str, str]:
    salt = bytes.fromhex(salt_hex) if salt_hex else secrets.token_bytes(16)
    digest = hashlib.scrypt(
        password.encode("utf-8"),
        salt=salt,
        n=16384,
        r=8,
        p=1,
        dklen=32,
    )
    return salt.hex(), digest.hex()


def verify_password(password: str, salt_hex: str, expected_hash_hex: str) -> bool:
    try:
        _, calc = hash_password(password, salt_hex)
        return hmac.compare_digest(calc, expected_hash_hex)
    except Exception:
        return False


def hash_token(token: str) -> str:
    return hashlib.sha256(token.encode("utf-8")).hexdigest()


def generate_invite_code() -> str:
    p1 = "".join(secrets.choice(INVITE_ALPHABET) for _ in range(4))
    p2 = "".join(secrets.choice(INVITE_ALPHABET) for _ in range(4))
    return f"BB-{p1}-{p2}"


class RateLimiter:
    """In-memory sliding-window rate limiter for auth endpoints."""

    def __init__(self, limit: int = 15, window_sec: int = 300) -> None:
        self.limit = limit
        self.window_sec = window_sec
        self._lock = threading.Lock()
        self._hits: dict[str, list[float]] = {}

    def allow(self, key: str) -> bool:
        now = time.monotonic()
        cutoff = now - self.window_sec
        with self._lock:
            arr = [t for t in self._hits.get(key, []) if t > cutoff]
            if len(arr) >= self.limit:
                self._hits[key] = arr
                return False
            arr.append(now)
            self._hits[key] = arr
            if len(self._hits) > 4096:
                stale = [k for k, v in self._hits.items() if not v or v[-1] <= cutoff]
                for k in stale:
                    self._hits.pop(k, None)
            return True

    def record_failure(self, key: str) -> None:
        self.allow(key)

    def check(self, key: str) -> bool:
        now = time.monotonic()
        cutoff = now - self.window_sec
        with self._lock:
            arr = [t for t in self._hits.get(key, []) if t > cutoff]
            self._hits[key] = arr
            return len(arr) < self.limit


class AppState:
    def __init__(self, db_path: Path, data_dir: Path, public_dir: Path) -> None:
        self.db_path = db_path
        self.data_dir = data_dir.resolve()
        self.public_dir = public_dir.resolve()
        self.auth_limiter = RateLimiter(limit=15, window_sec=300)
        self._db_init_lock = threading.Lock()
        self.init_db()

    def connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(str(self.db_path), timeout=15.0, isolation_level=None)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA foreign_keys = ON;")
        conn.execute("PRAGMA busy_timeout = 10000;")
        return conn

    def init_db(self) -> None:
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        with self._db_init_lock:
            conn = self.connect()
            try:
                conn.execute("PRAGMA journal_mode = WAL;")
                conn.executescript(
                    """
                    CREATE TABLE IF NOT EXISTS users (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        username TEXT NOT NULL UNIQUE COLLATE NOCASE,
                        pw_salt TEXT NOT NULL,
                        pw_hash TEXT NOT NULL,
                        is_admin INTEGER NOT NULL DEFAULT 0,
                        invite_code_used TEXT,
                        created_at TEXT NOT NULL
                    );
                    CREATE TABLE IF NOT EXISTS sessions (
                        token_hash TEXT PRIMARY KEY,
                        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                        expires_at INTEGER NOT NULL,
                        created_at TEXT NOT NULL
                    );
                    CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
                    CREATE INDEX IF NOT EXISTS idx_sessions_exp ON sessions(expires_at);

                    CREATE TABLE IF NOT EXISTS invite_codes (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        code TEXT NOT NULL UNIQUE COLLATE NOCASE,
                        note TEXT NOT NULL DEFAULT '',
                        max_uses INTEGER NOT NULL DEFAULT 1,
                        used_count INTEGER NOT NULL DEFAULT 0,
                        active INTEGER NOT NULL DEFAULT 1,
                        created_by INTEGER,
                        created_at TEXT NOT NULL
                    );

                    CREATE TABLE IF NOT EXISTS presets (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                        name TEXT NOT NULL,
                        summary TEXT NOT NULL DEFAULT '',
                        payload_json TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        updated_at TEXT NOT NULL
                    );
                    CREATE INDEX IF NOT EXISTS idx_presets_user ON presets(user_id);
                    """
                )
                try:
                    os.chmod(self.db_path, 0o600)
                except OSError:
                    pass

                row = conn.execute("SELECT COUNT(*) AS c FROM users").fetchone()
                if row and row["c"] == 0:
                    admin_user = os.environ.get("WTV_ADMIN_USER", "admin").strip() or "admin"
                    admin_pw = os.environ.get("WTV_ADMIN_INIT_PASSWORD", "").strip() or secrets.token_urlsafe(12)
                    init_code = os.environ.get("WTV_INIT_INVITE_CODE", "").strip().upper() or generate_invite_code()
                    salt, pwh = hash_password(admin_pw)
                    ts = now_iso()
                    conn.execute("BEGIN IMMEDIATE;")
                    cur = conn.execute(
                        "INSERT INTO users(username, pw_salt, pw_hash, is_admin, invite_code_used, created_at) VALUES (?, ?, ?, 1, 'SYSTEM-INIT', ?)",
                        (admin_user, salt, pwh, ts),
                    )
                    admin_id = cur.lastrowid
                    conn.execute(
                        "INSERT INTO invite_codes(code, note, max_uses, used_count, active, created_by, created_at) VALUES (?, ?, ?, 0, 1, ?, ?)",
                        (init_code, "初始邀请码（可在后台新建或停用）", 10, admin_id, ts),
                    )
                    conn.execute("COMMIT;")
                    receipt_path = self.db_path.parent / "bootstrap-admin.json"
                    receipt_path.write_text(
                        json.dumps(
                            {
                                "created_at": ts,
                                "admin_username": admin_user,
                                "admin_initial_password": admin_pw,
                                "initial_invite_code": init_code,
                                "initial_invite_max_uses": 10,
                            },
                            ensure_ascii=False,
                            indent=2,
                        ),
                        encoding="utf-8",
                    )
                    try:
                        os.chmod(receipt_path, 0o600)
                    except OSError:
                        pass
            finally:
                conn.close()


class ApiError(Exception):
    def __init__(self, status: int, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.message = message


class BallisticHandler(BaseHTTPRequestHandler):
    server_version = "BabyBallisticWeb/1.0"
    protocol_version = "HTTP/1.1"

    @property
    def app(self) -> AppState:
        return self.server.app  # type: ignore[attr-defined]

    def log_message(self, fmt: str, *args: Any) -> None:
        # Avoid logging sensitive bodies or tokens; keep concise access logs for API/errors
        msg = fmt % args
        if "/data/" in msg and " 200 " in msg or " 304 " in msg:
            return
        print(f"[{now_iso()}] {self.client_ip()} {msg}", flush=True)

    def client_ip(self) -> str:
        # Server binds only to 127.0.0.1 behind cloudflared, so CF-Connecting-IP is trustworthy
        cf_ip = self.headers.get("CF-Connecting-IP")
        if cf_ip:
            return cf_ip.strip()
        xff = self.headers.get("X-Forwarded-For")
        if xff:
            return xff.split(",")[0].strip()
        return self.client_address[0]

    def is_https(self) -> bool:
        return self.headers.get("X-Forwarded-Proto", "").lower() == "https"

    def add_security_headers(self) -> None:
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("X-Frame-Options", "DENY")
        self.send_header("Referrer-Policy", "same-origin")

    def send_json(self, status: int, payload: dict[str, Any], extra_headers: list[tuple[str, str]] | None = None) -> None:
        raw = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.add_security_headers()
        if extra_headers:
            for k, v in extra_headers:
                self.send_header(k, v)
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def read_json(self) -> dict[str, Any]:
        if self.headers.get("X-Requested-With") != "XMLHttpRequest":
            raise ApiError(HTTPStatus.FORBIDDEN, "缺少安全请求头，请刷新页面后重试")
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            raise ApiError(HTTPStatus.BAD_REQUEST, "无效的请求长度")
        if length <= 0 or length > MAX_BODY_BYTES:
            raise ApiError(HTTPStatus.BAD_REQUEST, "请求内容大小超出限制")
        body = self.rfile.read(length)
        try:
            obj = json.loads(body.decode("utf-8"))
        except Exception:
            raise ApiError(HTTPStatus.BAD_REQUEST, "请求 JSON 格式错误")
        if not isinstance(obj, dict):
            raise ApiError(HTTPStatus.BAD_REQUEST, "请求体必须是对象")
        return obj

    def get_session_token(self) -> str | None:
        raw_cookie = self.headers.get("Cookie")
        if not raw_cookie:
            return None
        jar = http.cookies.SimpleCookie()
        try:
            jar.load(raw_cookie)
        except Exception:
            return None
        morsel = jar.get(COOKIE_NAME)
        return morsel.value if morsel and morsel.value else None

    def make_cookie_header(self, token: str, max_age: int = SESSION_TTL_SECONDS) -> str:
        parts = [
            f"{COOKIE_NAME}={token}",
            "Path=/",
            f"Max-Age={max_age}",
            "HttpOnly",
            "SameSite=Lax",
        ]
        if self.is_https():
            parts.append("Secure")
        return "; ".join(parts)

    def current_user(self, conn: sqlite3.Connection) -> dict[str, Any] | None:
        token = self.get_session_token()
        if not token:
            return None
        th = hash_token(token)
        now_ts = int(time.time())
        row = conn.execute(
            """
            SELECT u.id, u.username, u.is_admin, s.expires_at
            FROM sessions s
            JOIN users u ON u.id = s.user_id
            WHERE s.token_hash = ?
            """,
            (th,),
        ).fetchone()
        if not row:
            return None
        if row["expires_at"] < now_ts:
            conn.execute("DELETE FROM sessions WHERE token_hash = ?", (th,))
            return None
        return {
            "id": row["id"],
            "username": row["username"],
            "isAdmin": bool(row["is_admin"]),
            "tokenHash": th,
        }

    def require_user(self, conn: sqlite3.Connection) -> dict[str, Any]:
        u = self.current_user(conn)
        if not u:
            raise ApiError(HTTPStatus.UNAUTHORIZED, "登录状态已失效，请重新登录")
        return u

    def require_admin(self, conn: sqlite3.Connection) -> dict[str, Any]:
        u = self.require_user(conn)
        if not u["isAdmin"]:
            raise ApiError(HTTPStatus.FORBIDDEN, "需要管理员权限")
        return u

    # ---------------------------- HTTP Verbs ----------------------------

    def do_HEAD(self) -> None:
        self._dispatch(head_only=True)

    def do_GET(self) -> None:
        self._dispatch(head_only=False)

    def do_POST(self) -> None:
        self._dispatch_api("POST")

    def do_PUT(self) -> None:
        self._dispatch_api("PUT")

    def do_DELETE(self) -> None:
        self._dispatch_api("DELETE")

    def _dispatch(self, head_only: bool) -> None:
        parsed = urllib.parse.urlsplit(self.path)
        path = parsed.path
        try:
            if path == "/api/health":
                self.send_json(HTTPStatus.OK, {"ok": True, "service": "baby-ballistic"})
                return
            if path.startswith("/api/"):
                if head_only:
                    self.send_error(HTTPStatus.METHOD_NOT_ALLOWED)
                    return
                self._handle_api_get(path)
                return
            if path in ("/", "/index.html"):
                self._serve_index(head_only)
                return
            if path in ("/icon.svg", "/favicon.ico"):
                self.send_response(HTTPStatus.OK)
                self.send_header("Content-Type", "image/svg+xml")
                self.send_header("Cache-Control", "public, max-age=86400")
                self.add_security_headers()
                self.send_header("Content-Length", str(len(APP_ICON_SVG)))
                self.end_headers()
                if not head_only:
                    self.wfile.write(APP_ICON_SVG)
                return
            if path == "/manifest.webmanifest":
                self.send_response(HTTPStatus.OK)
                self.send_header("Content-Type", "application/manifest+json; charset=utf-8")
                self.send_header("Cache-Control", "public, max-age=86400")
                self.add_security_headers()
                self.send_header("Content-Length", str(len(APP_MANIFEST_JSON)))
                self.end_headers()
                if not head_only:
                    self.wfile.write(APP_MANIFEST_JSON)
                return
            if path.startswith("/data/"):
                self._serve_data(path[len("/data/"):], head_only)
                return
            self.send_json(HTTPStatus.NOT_FOUND, {"error": "页面不存在"})
        except ApiError as e:
            self.send_json(e.status, {"error": e.message})
        except BrokenPipeError:
            pass
        except Exception as e:
            print(f"[ERROR] GET {path}: {e}", flush=True)
            self.send_json(HTTPStatus.INTERNAL_SERVER_ERROR, {"error": "服务器内部错误"})

    def _dispatch_api(self, method: str) -> None:
        parsed = urllib.parse.urlsplit(self.path)
        path = parsed.path
        try:
            if not path.startswith("/api/"):
                raise ApiError(HTTPStatus.NOT_FOUND, "接口不存在")
            self._handle_api_write(method, path)
        except ApiError as e:
            self.send_json(e.status, {"error": e.message})
        except BrokenPipeError:
            pass
        except Exception as e:
            print(f"[ERROR] {method} {path}: {e}", flush=True)
            self.send_json(HTTPStatus.INTERNAL_SERVER_ERROR, {"error": "服务器内部错误"})

    # ---------------------------- Static & Data ----------------------------

    def _serve_index(self, head_only: bool) -> None:
        idx_file = self.app.public_dir / "index.html"
        if not idx_file.is_file():
            raise ApiError(HTTPStatus.NOT_FOUND, "缺少前端页面文件")
        raw = idx_file.read_bytes()
        accept_gz = "gzip" in self.headers.get("Accept-Encoding", "")
        body = gzip.compress(raw, compresslevel=6) if accept_gz else raw
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Vary", "Accept-Encoding")
        if accept_gz:
            self.send_header("Content-Encoding", "gzip")
        self.add_security_headers()
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if not head_only:
            self.wfile.write(body)

    def _serve_data(self, rel_raw: str, head_only: bool) -> None:
        rel = urllib.parse.unquote(rel_raw).lstrip("/")
        if not rel or "\\" in rel or ".." in rel.split("/"):
            raise ApiError(HTTPStatus.BAD_REQUEST, "非法资源路径")

        conn = self.app.connect()
        try:
            self.require_user(conn)
        finally:
            conn.close()

        target = (self.app.data_dir / rel).resolve()
        if not target.is_relative_to(self.app.data_dir):
            raise ApiError(HTTPStatus.FORBIDDEN, "禁止访问该路径")

        gz_path = Path(str(target) + ".gz")
        has_gz = gz_path.is_file()
        has_raw = target.is_file()
        if not has_gz and not has_raw:
            raise ApiError(HTTPStatus.NOT_FOUND, f"数据文件不存在: {rel}")

        stat_file = gz_path if has_gz else target
        st = stat_file.stat()
        etag = f'W/"{st.st_size:x}-{int(st.st_mtime):x}"'
        if self.headers.get("If-None-Match") == etag:
            self.send_response(HTTPStatus.NOT_MODIFIED)
            self.send_header("ETag", etag)
            self.send_header("Cache-Control", "private, max-age=604800, immutable")
            self.add_security_headers()
            self.end_headers()
            return

        ext = target.suffix.lower()
        ctype = MIME_OVERRIDES.get(ext) or mimetypes.guess_type(str(target))[0] or "application/octet-stream"
        accept_gz = "gzip" in self.headers.get("Accept-Encoding", "")

        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", ctype)
        self.send_header("ETag", etag)
        self.send_header("Cache-Control", "private, max-age=604800, immutable")
        self.send_header("Vary", "Accept-Encoding")
        self.add_security_headers()

        if has_gz and accept_gz:
            self.send_header("Content-Encoding", "gzip")
            self.send_header("Content-Length", str(st.st_size))
            self.end_headers()
            if not head_only:
                with open(gz_path, "rb") as f:
                    shutil.copyfileobj(f, self.wfile, length=256 * 1024)
        elif has_gz and not has_raw:
            raw_bytes = gzip.decompress(gz_path.read_bytes())
            self.send_header("Content-Length", str(len(raw_bytes)))
            self.end_headers()
            if not head_only:
                self.wfile.write(raw_bytes)
        else:
            raw_st = target.stat()
            self.send_header("Content-Length", str(raw_st.st_size))
            self.end_headers()
            if not head_only:
                with open(target, "rb") as f:
                    shutil.copyfileobj(f, self.wfile, length=256 * 1024)

    # ---------------------------- API GET ----------------------------

    def _handle_api_get(self, path: str) -> None:
        conn = self.app.connect()
        try:
            if path == "/api/auth/me":
                u = self.current_user(conn)
                if not u:
                    self.send_json(HTTPStatus.OK, {"authenticated": False})
                    return
                cnt = conn.execute("SELECT COUNT(*) AS c FROM presets WHERE user_id = ?", (u["id"],)).fetchone()["c"]
                self.send_json(
                    HTTPStatus.OK,
                    {
                        "authenticated": True,
                        "user": {
                            "id": u["id"],
                            "username": u["username"],
                            "isAdmin": u["isAdmin"],
                            "presetCount": cnt,
                            "maxPresets": MAX_PRESETS_PER_USER,
                        },
                    },
                )
                return

            if path == "/api/presets":
                u = self.require_user(conn)
                rows = conn.execute(
                    "SELECT id, name, summary, payload_json, created_at, updated_at FROM presets WHERE user_id = ? ORDER BY id ASC",
                    (u["id"],),
                ).fetchall()
                presets = []
                for r in rows:
                    try:
                        data_obj = json.loads(r["payload_json"])
                    except Exception:
                        data_obj = {}
                    presets.append(
                        {
                            "id": r["id"],
                            "name": r["name"],
                            "summary": r["summary"],
                            "data": data_obj,
                            "createdAt": r["created_at"],
                            "updatedAt": r["updated_at"],
                        }
                    )
                self.send_json(
                    HTTPStatus.OK,
                    {"maxPresets": MAX_PRESETS_PER_USER, "presets": presets},
                )
                return

            if path == "/api/admin/overview":
                self.require_admin(conn)
                inv_rows = conn.execute(
                    "SELECT id, code, note, max_uses, used_count, active, created_at FROM invite_codes ORDER BY id DESC"
                ).fetchall()
                user_rows = conn.execute(
                    """
                    SELECT u.id, u.username, u.is_admin, u.invite_code_used, u.created_at,
                           COUNT(p.id) AS preset_count
                    FROM users u
                    LEFT JOIN presets p ON p.user_id = u.id
                    GROUP BY u.id
                    ORDER BY u.id ASC
                    """
                ).fetchall()
                used_by_map: dict[str, list[str]] = {}
                for ur in user_rows:
                    code_used = (ur["invite_code_used"] or "").upper()
                    if code_used:
                        used_by_map.setdefault(code_used, []).append(ur["username"])

                invites = [
                    {
                        "id": r["id"],
                        "code": r["code"],
                        "note": r["note"],
                        "maxUses": r["max_uses"],
                        "usedCount": r["used_count"],
                        "active": bool(r["active"]),
                        "usedBy": used_by_map.get(r["code"].upper(), []),
                        "createdAt": r["created_at"],
                    }
                    for r in inv_rows
                ]
                users = [
                    {
                        "id": r["id"],
                        "username": r["username"],
                        "isAdmin": bool(r["is_admin"]),
                        "inviteCodeUsed": r["invite_code_used"] or "",
                        "presetCount": r["preset_count"],
                        "createdAt": r["created_at"],
                    }
                    for r in user_rows
                ]
                self.send_json(HTTPStatus.OK, {"invites": invites, "users": users})
                return

            raise ApiError(HTTPStatus.NOT_FOUND, "接口不存在")
        finally:
            conn.close()

    # ---------------------------- API WRITE ----------------------------

    def _create_session(self, conn: sqlite3.Connection, user_id: int) -> str:
        raw_token = secrets.token_urlsafe(32)
        th = hash_token(raw_token)
        now_ts = int(time.time())
        conn.execute("DELETE FROM sessions WHERE expires_at < ?", (now_ts,))
        conn.execute(
            "INSERT INTO sessions(token_hash, user_id, expires_at, created_at) VALUES (?, ?, ?, ?)",
            (th, user_id, now_ts + SESSION_TTL_SECONDS, now_iso()),
        )
        return raw_token

    def _validate_preset_body(self, body: dict[str, Any]) -> tuple[str, str, str]:
        name = str(body.get("name", "")).strip()
        if not name or len(name) > 60:
            raise ApiError(HTTPStatus.BAD_REQUEST, "存档名称需在 1 到 60 个字符之间")
        summary = str(body.get("summary", "")).strip()[:140]
        data = body.get("data")
        if not isinstance(data, dict):
            raise ApiError(HTTPStatus.BAD_REQUEST, "存档参数格式无效")
        payload_json = json.dumps(data, ensure_ascii=False, separators=(",", ":"))
        if len(payload_json.encode("utf-8")) > MAX_PRESET_JSON_BYTES:
            raise ApiError(HTTPStatus.BAD_REQUEST, "存档参数体积超出限制")
        return name, summary, payload_json

    def _handle_api_write(self, method: str, path: str) -> None:
        ip = self.client_ip()
        conn = self.app.connect()
        try:
            # 1. Login
            if method == "POST" and path == "/api/auth/login":
                if not self.app.auth_limiter.check(ip):
                    raise ApiError(HTTPStatus.TOO_MANY_REQUESTS, "登录尝试过于频繁，请 5 分钟后再试")
                body = self.read_json()
                username = str(body.get("username", "")).strip()
                password = str(body.get("password", ""))
                if not username or not password:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "请输入用户名和密码")
                row = conn.execute(
                    "SELECT id, username, pw_salt, pw_hash, is_admin FROM users WHERE username = ?",
                    (username,),
                ).fetchone()
                if not row or not verify_password(password, row["pw_salt"], row["pw_hash"]):
                    self.app.auth_limiter.record_failure(ip)
                    raise ApiError(HTTPStatus.UNAUTHORIZED, "用户名或密码错误")
                token = self._create_session(conn, row["id"])
                cnt = conn.execute("SELECT COUNT(*) AS c FROM presets WHERE user_id = ?", (row["id"],)).fetchone()["c"]
                self.send_json(
                    HTTPStatus.OK,
                    {
                        "ok": True,
                        "user": {
                            "id": row["id"],
                            "username": row["username"],
                            "isAdmin": bool(row["is_admin"]),
                            "presetCount": cnt,
                            "maxPresets": MAX_PRESETS_PER_USER,
                        },
                    },
                    extra_headers=[("Set-Cookie", self.make_cookie_header(token))],
                )
                return

            # 2. Register with Admin Invite Code
            if method == "POST" and path == "/api/auth/register":
                if not self.app.auth_limiter.check(ip):
                    raise ApiError(HTTPStatus.TOO_MANY_REQUESTS, "尝试过于频繁，请 5 分钟后再试")
                body = self.read_json()
                username = str(body.get("username", "")).strip()
                password = str(body.get("password", ""))
                invite_code = str(body.get("inviteCode", "")).strip().upper()
                if not USERNAME_RE.match(username):
                    raise ApiError(HTTPStatus.BAD_REQUEST, "用户名需为 2~32 位中文、字母、数字、下划线或连字符")
                if len(password) < 6 or len(password) > 64:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "密码长度需在 6~64 位之间")
                if not invite_code:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "注册必须填写管理员邀请码")

                salt, pwh = hash_password(password)
                ts = now_iso()
                conn.execute("BEGIN IMMEDIATE;")
                try:
                    exists = conn.execute("SELECT 1 FROM users WHERE username = ?", (username,)).fetchone()
                    if exists:
                        conn.execute("ROLLBACK;")
                        raise ApiError(HTTPStatus.CONFLICT, "该用户名已被注册，请换一个")

                    inv = conn.execute(
                        "SELECT id, code, max_uses, used_count, active FROM invite_codes WHERE code = ?",
                        (invite_code,),
                    ).fetchone()
                    if not inv:
                        conn.execute("ROLLBACK;")
                        self.app.auth_limiter.record_failure(ip)
                        raise ApiError(HTTPStatus.BAD_REQUEST, "邀请码不存在，请核对后重试")
                    if not inv["active"]:
                        conn.execute("ROLLBACK;")
                        raise ApiError(HTTPStatus.BAD_REQUEST, "该邀请码已被管理员停用")
                    if inv["max_uses"] > 0 and inv["used_count"] >= inv["max_uses"]:
                        conn.execute("ROLLBACK;")
                        raise ApiError(HTTPStatus.BAD_REQUEST, "该邀请码已达到使用次数上限")

                    upd = conn.execute(
                        "UPDATE invite_codes SET used_count = used_count + 1 WHERE id = ? AND active = 1 AND (max_uses = 0 OR used_count < max_uses)",
                        (inv["id"],),
                    )
                    if upd.rowcount != 1:
                        conn.execute("ROLLBACK;")
                        raise ApiError(HTTPStatus.BAD_REQUEST, "邀请码刚刚已被用完")

                    cur = conn.execute(
                        "INSERT INTO users(username, pw_salt, pw_hash, is_admin, invite_code_used, created_at) VALUES (?, ?, ?, 0, ?, ?)",
                        (username, salt, pwh, inv["code"], ts),
                    )
                    user_id = int(cur.lastrowid)
                    conn.execute("COMMIT;")
                except ApiError:
                    raise
                except Exception:
                    conn.execute("ROLLBACK;")
                    raise

                token = self._create_session(conn, user_id)
                self.send_json(
                    HTTPStatus.CREATED,
                    {
                        "ok": True,
                        "user": {
                            "id": user_id,
                            "username": username,
                            "isAdmin": False,
                            "presetCount": 0,
                            "maxPresets": MAX_PRESETS_PER_USER,
                        },
                    },
                    extra_headers=[("Set-Cookie", self.make_cookie_header(token))],
                )
                return

            # 3. Logout
            if method == "POST" and path == "/api/auth/logout":
                _ = self.read_json()
                u = self.current_user(conn)
                if u:
                    conn.execute("DELETE FROM sessions WHERE token_hash = ?", (u["tokenHash"],))
                self.send_json(
                    HTTPStatus.OK,
                    {"ok": True},
                    extra_headers=[("Set-Cookie", self.make_cookie_header("", max_age=0))],
                )
                return

            # 4. Change own password
            if method == "POST" and path == "/api/auth/password":
                u = self.require_user(conn)
                body = self.read_json()
                old_pw = str(body.get("oldPassword", ""))
                new_pw = str(body.get("newPassword", ""))
                if len(new_pw) < 6 or len(new_pw) > 64:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "新密码长度需在 6~64 位之间")
                row = conn.execute("SELECT pw_salt, pw_hash FROM users WHERE id = ?", (u["id"],)).fetchone()
                if not row or not verify_password(old_pw, row["pw_salt"], row["pw_hash"]):
                    raise ApiError(HTTPStatus.BAD_REQUEST, "当前原密码不正确")
                salt, pwh = hash_password(new_pw)
                conn.execute("UPDATE users SET pw_salt = ?, pw_hash = ? WHERE id = ?", (salt, pwh, u["id"]))
                conn.execute("DELETE FROM sessions WHERE user_id = ? AND token_hash != ?", (u["id"], u["tokenHash"]))
                self.send_json(HTTPStatus.OK, {"ok": True})
                return

            # 5. Create preset (strictly <= 30 per user)
            if method == "POST" and path == "/api/presets":
                u = self.require_user(conn)
                body = self.read_json()
                name, summary, payload_json = self._validate_preset_body(body)
                ts = now_iso()
                conn.execute("BEGIN IMMEDIATE;")
                try:
                    cnt = conn.execute("SELECT COUNT(*) AS c FROM presets WHERE user_id = ?", (u["id"],)).fetchone()["c"]
                    if cnt >= MAX_PRESETS_PER_USER:
                        conn.execute("ROLLBACK;")
                        raise ApiError(
                            HTTPStatus.BAD_REQUEST,
                            f"每个账号最多可存 {MAX_PRESETS_PER_USER} 个档（当前已满 {cnt}/{MAX_PRESETS_PER_USER}），请覆盖已有存档或先删掉旧档",
                        )
                    cur = conn.execute(
                        "INSERT INTO presets(user_id, name, summary, payload_json, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
                        (u["id"], name, summary, payload_json, ts, ts),
                    )
                    pid = int(cur.lastrowid)
                    conn.execute("COMMIT;")
                except ApiError:
                    raise
                except Exception:
                    conn.execute("ROLLBACK;")
                    raise
                self.send_json(
                    HTTPStatus.CREATED,
                    {
                        "ok": True,
                        "preset": {
                            "id": pid,
                            "name": name,
                            "summary": summary,
                            "data": json.loads(payload_json),
                            "createdAt": ts,
                            "updatedAt": ts,
                        },
                    },
                )
                return

            # 6. Update / Delete preset
            m_preset = re.match(r"^/api/presets/(\d+)$", path)
            if m_preset:
                u = self.require_user(conn)
                pid = int(m_preset.group(1))
                if method == "PUT":
                    body = self.read_json()
                    name, summary, payload_json = self._validate_preset_body(body)
                    ts = now_iso()
                    upd = conn.execute(
                        "UPDATE presets SET name = ?, summary = ?, payload_json = ?, updated_at = ? WHERE id = ? AND user_id = ?",
                        (name, summary, payload_json, ts, pid, u["id"]),
                    )
                    if upd.rowcount == 0:
                        raise ApiError(HTTPStatus.NOT_FOUND, "未找到该存档")
                    self.send_json(
                        HTTPStatus.OK,
                        {
                            "ok": True,
                            "preset": {
                                "id": pid,
                                "name": name,
                                "summary": summary,
                                "data": json.loads(payload_json),
                                "updatedAt": ts,
                            },
                        },
                    )
                    return
                if method == "DELETE":
                    _ = self.read_json()
                    dl = conn.execute("DELETE FROM presets WHERE id = ? AND user_id = ?", (pid, u["id"]))
                    if dl.rowcount == 0:
                        raise ApiError(HTTPStatus.NOT_FOUND, "未找到该存档")
                    self.send_json(HTTPStatus.OK, {"ok": True})
                    return

            # 7. Admin: Create Invite Code
            if method == "POST" and path == "/api/admin/invites":
                adm = self.require_admin(conn)
                body = self.read_json()
                try:
                    max_uses = int(body.get("maxUses", 1))
                except (TypeError, ValueError):
                    raise ApiError(HTTPStatus.BAD_REQUEST, "可用次数必须是整数")
                if max_uses < 0 or max_uses > 10000:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "可用次数范围应为 0（不限）到 10000")
                note = str(body.get("note", "")).strip()[:80]
                custom = str(body.get("customCode", "")).strip().upper()
                if custom:
                    if not re.match(r"^[A-Z0-9\-]{4,32}$", custom):
                        raise ApiError(HTTPStatus.BAD_REQUEST, "自定义邀请码仅限 4~32 位大写字母、数字或连字符")
                    code = custom
                else:
                    code = generate_invite_code()
                ts = now_iso()
                try:
                    cur = conn.execute(
                        "INSERT INTO invite_codes(code, note, max_uses, used_count, active, created_by, created_at) VALUES (?, ?, ?, 0, 1, ?, ?)",
                        (code, note, max_uses, adm["id"], ts),
                    )
                except sqlite3.IntegrityError:
                    raise ApiError(HTTPStatus.CONFLICT, "该邀请码已存在，请换一个")
                self.send_json(
                    HTTPStatus.CREATED,
                    {
                        "ok": True,
                        "invite": {
                            "id": int(cur.lastrowid),
                            "code": code,
                            "note": note,
                            "maxUses": max_uses,
                            "usedCount": 0,
                            "active": True,
                            "usedBy": [],
                            "createdAt": ts,
                        },
                    },
                )
                return

            # 8. Admin: Toggle / Delete Invite Code
            m_inv_toggle = re.match(r"^/api/admin/invites/(\d+)/toggle$", path)
            if method == "POST" and m_inv_toggle:
                self.require_admin(conn)
                _ = self.read_json()
                iid = int(m_inv_toggle.group(1))
                row = conn.execute("SELECT active FROM invite_codes WHERE id = ?", (iid,)).fetchone()
                if not row:
                    raise ApiError(HTTPStatus.NOT_FOUND, "邀请码不存在")
                new_active = 0 if row["active"] else 1
                conn.execute("UPDATE invite_codes SET active = ? WHERE id = ?", (new_active, iid))
                self.send_json(HTTPStatus.OK, {"ok": True, "active": bool(new_active)})
                return

            m_inv_del = re.match(r"^/api/admin/invites/(\d+)$", path)
            if method == "DELETE" and m_inv_del:
                self.require_admin(conn)
                _ = self.read_json()
                iid = int(m_inv_del.group(1))
                conn.execute("DELETE FROM invite_codes WHERE id = ?", (iid,))
                self.send_json(HTTPStatus.OK, {"ok": True})
                return

            # 9. Admin: Reset User Password / Delete User
            m_usr_pw = re.match(r"^/api/admin/users/(\d+)/reset-password$", path)
            if method == "POST" and m_usr_pw:
                self.require_admin(conn)
                body = self.read_json()
                uid = int(m_usr_pw.group(1))
                row = conn.execute("SELECT id, username FROM users WHERE id = ?", (uid,)).fetchone()
                if not row:
                    raise ApiError(HTTPStatus.NOT_FOUND, "用户不存在")
                new_pw = str(body.get("newPassword", "")).strip() or "".join(
                    secrets.choice(INVITE_ALPHABET) for _ in range(10)
                )
                if len(new_pw) < 6 or len(new_pw) > 64:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "密码长度需在 6~64 位之间")
                salt, pwh = hash_password(new_pw)
                conn.execute("UPDATE users SET pw_salt = ?, pw_hash = ? WHERE id = ?", (salt, pwh, uid))
                conn.execute("DELETE FROM sessions WHERE user_id = ?", (uid,))
                self.send_json(HTTPStatus.OK, {"ok": True, "username": row["username"], "newPassword": new_pw})
                return

            m_usr_del = re.match(r"^/api/admin/users/(\d+)$", path)
            if method == "DELETE" and m_usr_del:
                adm = self.require_admin(conn)
                _ = self.read_json()
                uid = int(m_usr_del.group(1))
                if uid == adm["id"]:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "不能删除当前正在使用的管理员账号")
                row = conn.execute("SELECT is_admin FROM users WHERE id = ?", (uid,)).fetchone()
                if not row:
                    raise ApiError(HTTPStatus.NOT_FOUND, "用户不存在")
                if row["is_admin"]:
                    raise ApiError(HTTPStatus.BAD_REQUEST, "不能删除管理员账号")
                conn.execute("DELETE FROM users WHERE id = ?", (uid,))
                self.send_json(HTTPStatus.OK, {"ok": True})
                return

            raise ApiError(HTTPStatus.NOT_FOUND, "接口不存在")
        finally:
            conn.close()


def main() -> None:
    root = Path(__file__).resolve().parent
    ap = argparse.ArgumentParser(description="婴儿弹道计算机 Web 服务")
    ap.add_argument("--host", default=os.environ.get("WTV_WEB_HOST", "127.0.0.1"))
    ap.add_argument("--port", type=int, default=int(os.environ.get("WTV_WEB_PORT", "18820")))
    ap.add_argument("--data-dir", default=os.environ.get("WTV_DATA_DIR", str(root / "data")))
    ap.add_argument("--public-dir", default=os.environ.get("WTV_PUBLIC_DIR", str(root / "public")))
    ap.add_argument("--db-path", default=os.environ.get("WTV_DB_PATH", str(root / "var" / "app.db")))
    args = ap.parse_args()

    state = AppState(
        db_path=Path(args.db_path),
        data_dir=Path(args.data_dir),
        public_dir=Path(args.public_dir),
    )
    httpd = ThreadingHTTPServer((args.host, args.port), BallisticHandler)
    httpd.daemon_threads = True
    httpd.app = state  # type: ignore[attr-defined]
    print(f"婴儿弹道计算机 Web listening on http://{args.host}:{args.port} (data={state.data_dir}, db={state.db_path})", flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        httpd.server_close()


if __name__ == "__main__":
    main()
