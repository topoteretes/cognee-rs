#!/usr/bin/env python3
"""
Generate test fixtures for cross-SDK JWT and password hash compat tests.

Run once to (re)generate the checked-in fixtures:

    cd crates/http-server/tests/fixtures/auth
    python3 gen.py

Requires:
    pip install python-jose[cryptography] bcrypt argon2-cffi
"""

import json
import time

# ─── Parameters (must match Rust side) ──────────────────────────────────────

SECRET = "super_secret"
SUB = "12345678-1234-5678-1234-567812345678"
# Fixed timestamp so the token is deterministic.
# 2025-01-01T00:00:00 UTC = 1735689600
IAT = 1735689600
LIFETIME = 3600
PASSWORD = "correct horse battery staple"

# ─── Login JWT ───────────────────────────────────────────────────────────────

from jose import jwt as jose_jwt

login_claims = {
    "sub": SUB,
    "aud": ["fastapi-users:auth"],
    "exp": IAT + LIFETIME,
    "iat": IAT,
}
login_token = jose_jwt.encode(login_claims, SECRET, algorithm="HS256")
print(f"Login JWT: {login_token}")

with open("python_login_jwt.txt", "w") as f:
    f.write(login_token.strip())

# Write metadata alongside for documentation
with open("python_login_jwt_meta.json", "w") as f:
    json.dump({
        "secret": SECRET,
        "sub": SUB,
        "iat": IAT,
        "exp": IAT + LIFETIME,
        "aud": ["fastapi-users:auth"],
        "algorithm": "HS256",
    }, f, indent=2)

print("Wrote python_login_jwt.txt")

# ─── bcrypt hash ─────────────────────────────────────────────────────────────

import bcrypt

bcrypt_hash = bcrypt.hashpw(PASSWORD.encode(), bcrypt.gensalt(rounds=12)).decode()
print(f"bcrypt hash: {bcrypt_hash}")

with open("python_bcrypt_hash.txt", "w") as f:
    f.write(bcrypt_hash.strip())

print("Wrote python_bcrypt_hash.txt")

# ─── argon2id hash ───────────────────────────────────────────────────────────

from argon2 import PasswordHasher
from argon2.low_level import Type

ph = PasswordHasher(
    memory_cost=19456,
    time_cost=2,
    parallelism=1,
    hash_len=32,
    salt_len=16,
    encoding="utf-8",
)
argon2_hash = ph.hash(PASSWORD)
print(f"argon2id hash: {argon2_hash}")

with open("python_argon2_hash.txt", "w") as f:
    f.write(argon2_hash.strip())

print("Wrote python_argon2_hash.txt")
print("Done.")
