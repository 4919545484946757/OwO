#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""插件签名工具（M4b）：Ed25519 密钥生成 / 签名 / 校验。

摘要口径与 core::plugin::plugin_digest 一致（Rust 端校验）：
    sha256(id + "|" + name + "|" + version + "|" + entry_path + ["|" + entry_file_content])
（signature 字段本身不参与摘要，签名后才写入 manifest。）

用法：
  plugin-sign.py generate --key-file <path>            # 生成 Ed25519 密钥对（PEM）
  plugin-sign.py sign --plugin-dir <dir> --key-file <path>   # 签名并写回 manifest.json
  plugin-sign.py verify --plugin-dir <dir> [--key-file <path>]  # 校验（无 key-file 时从 manifest 公钥验）
  plugin-sign.py verify --plugin-dir <dir> --public-key-b64 <b64>  # 指定公钥校验
"""
import argparse
import base64
import hashlib
import json
import pathlib
import sys

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import (
    Ed25519PrivateKey,
    Ed25519PublicKey,
)


def plugin_digest(manifest: dict, entry_content: bytes | None) -> bytes:
    hasher = hashlib.sha256()
    parts = [
        manifest.get("id", "").encode("utf-8"),
        b"|",
        manifest.get("name", "").encode("utf-8"),
        b"|",
        manifest.get("version", "").encode("utf-8"),
        b"|",
        (manifest.get("entry") or "").encode("utf-8"),
    ]
    if entry_content is not None:
        parts.append(b"|")
        parts.append(entry_content)
    for part in parts:
        hasher.update(part)
    return hasher.digest()


def load_entry(plugin_dir: pathlib.Path, manifest: dict) -> bytes | None:
    entry = manifest.get("entry")
    if not entry:
        return None
    path = plugin_dir / entry
    if not path.exists():
        sys.stderr.write(f"[warn] 入口文件不存在：{path}\n")
        return None
    return path.read_bytes()


def generate(args) -> int:
    key = Ed25519PrivateKey.generate()
    pem = key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    )
    pathlib.Path(args.key_file).write_bytes(pem)
    print(f"已生成私钥：{args.key_file}（公钥 base64：{public_b64(key)})")
    return 0


def public_b64(key: Ed25519PrivateKey) -> str:
    return base64.b64encode(key.public_key().public_bytes(
        serialization.Encoding.Raw,
        serialization.PublicFormat.Raw,
    )).decode("ascii")


def sign(args) -> int:
    plugin_dir = pathlib.Path(args.plugin_dir)
    manifest_path = plugin_dir / "manifest.json"
    manifest = json.loads(manifest_path.read_bytes())
    entry = load_entry(plugin_dir, manifest)
    digest = plugin_digest(manifest, entry)

    key = Ed25519PrivateKey.from_private_bytes(
        serialization.load_pem_private_key(pathlib.Path(args.key_file).read_bytes(), password=None)
        .private_bytes(
            serialization.Encoding.Raw,
            serialization.PrivateFormat.Raw,
            serialization.NoEncryption(),
        )
    )
    signature = key.sign(digest)
    manifest["signature"] = {
        "algorithm": "ed25519",
        "public_key_b64": public_b64(key),
        "signature_b64": base64.b64encode(signature).decode("ascii"),
    }
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"已签名：{manifest.get('id')} v{manifest.get('version')}（写入 signature 字段）")
    return 0


def verify(args) -> int:
    plugin_dir = pathlib.Path(args.plugin_dir)
    manifest_path = plugin_dir / "manifest.json"
    manifest = json.loads(manifest_path.read_bytes())
    signature = manifest.get("signature")
    if not signature:
        print("校验失败：manifest 无 signature 字段", file=sys.stderr)
        return 1
    entry = load_entry(plugin_dir, manifest)
    digest = plugin_digest(manifest, entry)

    if args.public_key_b64:
        pub_b64 = args.public_key_b64
    elif args.key_file:
        key = Ed25519PrivateKey.from_private_bytes(
            serialization.load_pem_private_key(pathlib.Path(args.key_file).read_bytes(), password=None)
            .private_bytes(
                serialization.Encoding.Raw,
                serialization.PrivateFormat.Raw,
                serialization.NoEncryption(),
            )
        )
        pub_b64 = public_b64(key)
    else:
        pub_b64 = signature.get("public_key_b64")
    if not pub_b64:
        print("校验失败：缺少公钥（--key-file 或 --public-key-b64）", file=sys.stderr)
        return 1

    pub = Ed25519PublicKey.from_public_bytes(base64.b64decode(pub_b64))
    try:
        pub.verify(base64.b64decode(signature["signature_b64"]), digest)
    except Exception as exc:
        print(f"校验失败：签名不匹配（内容可能被篡改）：{exc}", file=sys.stderr)
        return 1
    print("校验通过：签名有效（Ed25519）")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="插件签名工具（Ed25519）")
    sub = parser.add_subparsers(dest="action", required=True)
    g = sub.add_parser("generate")
    g.add_argument("--key-file", required=True)
    s = sub.add_parser("sign")
    s.add_argument("--plugin-dir", required=True)
    s.add_argument("--key-file", required=True)
    v = sub.add_parser("verify")
    v.add_argument("--plugin-dir", required=True)
    v.add_argument("--key-file", default=None)
    v.add_argument("--public-key-b64", default=None)
    args = parser.parse_args()
    return {"generate": generate, "sign": sign, "verify": verify}[args.action](args)


if __name__ == "__main__":
    sys.exit(main())
