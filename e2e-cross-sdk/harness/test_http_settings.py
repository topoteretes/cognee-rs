"""Phase-1a parity tests for /api/v1/settings (no LLM key required).

This is the guard that `docs/http-server/routers/settings.md` §5 item 8 asks
for and that `docs/roadmap/bedrock-provider-plan.md` §5 P4 tracks: it keeps the
server-rendered provider/model lists and the save-side provider enum from
re-diverging between the two SDKs.  `test_http_openapi.py` does not cover this
— it compares path/method/security-scheme/``components.schemas`` **key sets**
only, so an enum value or a model-list entry can drift without tripping it.

Authentication is asymmetric here, and deliberately so.  The Rust server serves
``/settings`` unauthenticated: ``require_authentication`` defaults to false
(``crates/http-server/src/config.rs``) and ``bin/start_servers.sh`` does not set
``REQUIRE_AUTHENTICATION``, so ``AuthenticatedUser`` resolves to the synthetic
default user.  Upstream Python at the SHA pinned in
``.github/workflows/http-parity.yml`` does **not**: its
``get_authenticated_user`` computes

    REQUIRE_AUTHENTICATION = (
        os.getenv("REQUIRE_AUTHENTICATION", "true").lower() == "true"
        or os.environ.get("ENABLE_BACKEND_ACCESS_CONTROL", "true").lower() == "true"
    )

which is **true when both variables are unset** — the harness sets neither — so
an unauthenticated ``GET /api/v1/settings`` answers 401 rather than falling back
to ``get_default_user()``.  The ``py_settings_client`` fixture below therefore
logs the Python client in; see its docstring for why ``conftest.authed_clients``
is the wrong tool.

Ignore extension: the environment-dependent scalars.  ``llm.provider`` /
``model`` / ``endpoint`` / ``apiVersion`` / ``apiKey`` and ``vectorDb``'s
``provider`` / ``url`` / ``apiKey`` come from each server's own environment, and
the two servers run out of the separate ``/py`` and ``/rs`` tmpfs workspaces, so
``vectorDb.url`` in particular can never match (and ``start_servers.sh`` pins the
Rust server to ``VECTOR_DB_PROVIDER=mock``).  Both sides mask keys identically
(``key[:10] + "*" * (len(key) - 10)``), but the *configured* key is not
guaranteed to be the same, so the masked value is ignored rather than normalised.

Two notes on the ignore syntax, both load-bearing:

* ``strip_paths`` understands ``$.key``, ``$..key`` and ``$.list[*].key`` only —
  a two-level ``$.llm.api_key`` parses as an unknown pattern and is silently
  dropped, i.e. it would *not* ignore anything.  The recursive ``$..`` form is
  used instead, and it is exact for this endpoint: nothing nested inside the
  response reuses these key names (the entries of ``providers`` / ``models``
  carry ``value`` and ``label`` only).
* The wire keys are **camelCase on both sides**, so the recursive patterns must
  spell them that way.  Rust's DTOs carry ``#[serde(rename_all = "camelCase")]``
  (enforced by ``crates/http-server/tests/test_openapi_camelcase.rs``) and
  Python's ``OutDTO`` sets ``alias_generator=to_camel`` with FastAPI's default
  ``response_model_by_alias=True``.  Both therefore emit ``apiKey`` /
  ``apiVersion`` / ``vectorDb``.  The snake_case spellings are kept alongside so
  the ignore set survives either side flipping its serialization convention.
"""

import pytest

from http_helpers import DEFAULT_IGNORE, assert_responses_match

SETTINGS_PATH = "/api/v1/settings"

# Environment-dependent scalars — see the module docstring for why each one is
# here, why the patterns are `$..` rather than `$.llm.…`, and why both casings
# are listed.
_SETTINGS_IGNORE = DEFAULT_IGNORE | {
    # llm.apiKey + vectorDb.apiKey (masked, but the configured key may differ)
    "$..apiKey",
    "$..api_key",
    # llm.apiVersion (env-configured)
    "$..apiVersion",
    "$..api_version",
    "$..provider",  # llm.provider + vectorDb.provider (env-configured)
    "$..model",  # llm.model (env-configured; the `models` map is NOT matched)
    "$..endpoint",  # llm.endpoint (env-configured)
    "$..url",  # vectorDb.url — /py vs /rs tmpfs workspaces
}

# Known, pre-existing, NON-bedrock list divergences.  Out of scope for the
# bedrock plan (which only aligns the bedrock entries); enumerated here because
# they are what keeps `test_settings_get_full_body_parity` xfailing.
_KNOWN_LIST_DIVERGENCES = (
    "llm.models.openai (py: gpt-5-mini/gpt-4o/gpt-4-turbo/gpt-3.5-turbo vs "
    "rs: gpt-4o/gpt-4o-mini/gpt-4-turbo/gpt-4/gpt-3.5-turbo); "
    "llm.models.anthropic (py: Claude 3 Opus/Sonnet/Haiku vs "
    "rs: claude-3-5-sonnet-latest/claude-3-opus-latest); "
    "llm.models.gemini (py: gemini-2.0-flash-exp labelled 'Gemini 2.0 Flash' vs "
    "rs: gemini-2.0-flash/gemini-1.5-pro); "
    "llm.models.mistral (py: four dated models vs "
    "rs: mistral-large-latest/open-mistral-nemo); "
    "vector_db.providers (py: [LanceDB, PGVector] vs "
    "rs: [LanceDB, pgvector, brute-force] — extra entry and label case)"
)

# Credentials for the Python-side session.  Mirrors the pair
# `conftest.authed_clients` uses; the email is module-specific so a failure here
# is never confused with that fixture's user.
_PY_CREDS = {"username": "settings-parity@example.com", "password": "test_password_123"}


# ── Fixtures ─────────────────────────────────────────────────────────────────


@pytest.fixture
def py_settings_client(py_client):
    """``py_client`` with a logged-in session cookie.

    Upstream Python requires authentication on ``/api/v1/settings`` (see the
    module docstring), so every Python-side call in this file goes through here.

    ``conftest.authed_clients`` is deliberately **not** used: it calls
    ``pytest.skip`` whenever the session-scoped ``auth_endpoints_available``
    probe finds ``/api/v1/auth/*`` missing — which is always true for the *Rust*
    OSS ``cognee-http-server``.  Skipping on that would silently void this gate
    in CI, which is the whole failure mode P4 exists to prevent.  The Rust client
    needs no session at all, so only the Python one is authenticated.

    The login is asserted rather than skipped: Python's ``/api/v1/auth/*`` routes
    are unconditionally mounted by ``cognee/api/client.py``, so a failure here is
    a real regression in the harness, not an unsupported deployment.
    """
    # Bootstrap the user; a re-run answers 400 REGISTER_USER_ALREADY_EXISTS,
    # which is fine — the login below is the actual gate.
    py_client.post(
        "/api/v1/auth/register",
        json={
            "email": _PY_CREDS["username"],
            "password": _PY_CREDS["password"],
            "is_verified": True,
        },
    )
    r = py_client.post("/api/v1/auth/login", data=_PY_CREDS)
    assert r.status_code == 200, (
        f"py: POST /api/v1/auth/login -> {r.status_code}: {r.text[:300]}. "
        "The Python settings endpoints require authentication at the pinned SHA."
    )
    return py_client


# ── Helpers ──────────────────────────────────────────────────────────────────


def _get_settings(client, side: str) -> dict:
    """GET /api/v1/settings, assert 200, return the parsed body."""
    r = client.get(SETTINGS_PATH)
    assert r.status_code == 200, f"{side}: GET {SETTINGS_PATH} -> {r.status_code}: {r.text[:300]}"
    return r.json()


def _provider_choice(body: dict, value: str, side: str) -> dict:
    """Return the ``llm.providers`` entry whose ``value`` is *value*."""
    providers = body["llm"]["providers"]
    matches = [p for p in providers if p.get("value") == value]
    assert matches, (
        f"{side}: llm.providers does not advertise {value!r}; "
        f"got {[p.get('value') for p in providers]}"
    )
    assert len(matches) == 1, f"{side}: llm.providers lists {value!r} {len(matches)} times"
    return matches[0]


# ── GET parity ───────────────────────────────────────────────────────────────


def test_settings_bedrock_provider_choice_matches(py_settings_client, rs_client):
    """The `bedrock` entry of llm.providers is identical (value AND label)."""
    py_choice = _provider_choice(_get_settings(py_settings_client, "py"), "bedrock", "py")
    rs_choice = _provider_choice(_get_settings(rs_client, "rs"), "bedrock", "rs")
    assert py_choice == rs_choice, (
        f"bedrock provider choice diverged: py={py_choice} rs={rs_choice}"
    )


def test_settings_bedrock_model_list_matches(py_settings_client, rs_client):
    """llm.models["bedrock"] matches element-for-element and in order.

    This is the drift guard for plan §5 P2: Rust carried a single stale
    `anthropic.claude-3-5-sonnet-20240620-v1:0` while Python moved to three
    `eu.*` models, and nothing caught it.
    """
    py_models = _get_settings(py_settings_client, "py")["llm"]["models"]
    rs_models = _get_settings(rs_client, "rs")["llm"]["models"]

    assert "bedrock" in py_models, f"py: llm.models has no bedrock key: {sorted(py_models)}"
    assert "bedrock" in rs_models, f"rs: llm.models has no bedrock key: {sorted(rs_models)}"
    assert py_models["bedrock"] == rs_models["bedrock"], (
        f"bedrock model list diverged:\npy={py_models['bedrock']}\nrs={rs_models['bedrock']}"
    )


def test_settings_llm_provider_value_set_matches(py_settings_client, rs_client):
    """llm.providers[*].value is the same list, in the same order, on both SDKs.

    Catches any future one-sided provider addition (the failure mode P1/P2/P3
    were all instances of).
    """
    py_values = [p.get("value") for p in _get_settings(py_settings_client, "py")["llm"]["providers"]]
    rs_values = [p.get("value") for p in _get_settings(rs_client, "rs")["llm"]["providers"]]

    assert set(py_values) == set(rs_values), (
        f"llm provider value sets diverged: only in py={sorted(set(py_values) - set(rs_values))}, "
        f"only in rs={sorted(set(rs_values) - set(py_values))}"
    )
    assert py_values == rs_values, (
        f"llm provider order diverged: py={py_values} rs={rs_values}"
    )


@pytest.mark.xfail(
    reason=(
        "Full-body GET parity is blocked by pre-existing, non-bedrock list "
        "divergences that are out of scope for bedrock-provider-plan.md §5: "
        + _KNOWN_LIST_DIVERGENCES
        + ". The environment-dependent scalars are already ignored (see the "
        "module docstring); everything left is a real list divergence. Delete "
        "this marker once those lists are aligned — strict=False means the day "
        "they converge this reports XPASS rather than turning CI red."
    ),
    strict=False,
)
def test_settings_get_full_body_parity(py_settings_client, rs_client):
    """GET /api/v1/settings is byte-equal modulo the env-dependent scalars."""
    py = py_settings_client.get(SETTINGS_PATH)
    rs = rs_client.get(SETTINGS_PATH)
    assert_responses_match(py, rs, ignore=_SETTINGS_IGNORE)


# ── POST parity ──────────────────────────────────────────────────────────────

# `api_key` is required by both input DTOs. The literal below deliberately
# contains no "*****" substring: both SDKs treat a submitted key containing
# that sentinel as the redacted echo from GET and refuse to persist it, so a
# sentinel-free value is what actually exercises the save path.
#
# The snake_case spelling is accepted by both sides on *input* — Rust's
# `LLMConfigInputDTO` carries `#[serde(alias = "api_key")]` and Python's `InDTO`
# sets `populate_by_name=True` — so it does not need the camelCase treatment the
# GET response does.
_BEDROCK_POST = {
    "llm": {
        "provider": "bedrock",
        "model": "eu.amazon.nova-lite-v1:0",
        "api_key": "test-key-not-persisted",
    }
}

# Restoring the previous provider/model afterwards uses this literal key: it
# contains the "*****" sentinel, so neither SDK overwrites the real configured
# key while the provider/model are put back.
_ECHO_SENTINEL_KEY = "*****"


def test_settings_post_bedrock_accepted_by_rust(rs_client):
    """POST provider="bedrock" is accepted by Rust and reflected on the next GET.

    The save mutates a process-singleton on the Rust server (Python does the
    same — settings are in-process, never written to a relational table), so
    this leaves `llm.provider`/`llm.model` changed for the rest of the session.
    Those two fields are in `_SETTINGS_IGNORE` for exactly that reason; the
    previous values are restored best-effort below anyway.
    """
    before = _get_settings(rs_client, "rs")["llm"]
    try:
        r = rs_client.post(SETTINGS_PATH, json=_BEDROCK_POST)
        assert r.status_code == 200, (
            f"rs: POST {SETTINGS_PATH} provider=bedrock -> {r.status_code}: {r.text[:300]}"
        )

        after = _get_settings(rs_client, "rs")["llm"]
        assert after["provider"] == "bedrock", (
            f"rs: provider did not persist as 'bedrock', got {after['provider']!r}"
        )
    finally:
        # Best-effort restore — unasserted: if the environment configured a
        # provider outside the input enum this POST is rejected, which is not
        # this test's subject.
        rs_client.post(
            SETTINGS_PATH,
            json={
                "llm": {
                    "provider": before["provider"],
                    "model": before["model"],
                    "api_key": _ECHO_SENTINEL_KEY,
                }
            },
        )


@pytest.mark.xfail(
    reason=(
        "Upstream gap, tracked as bedrock-provider-plan.md §5 P1: "
        "cognee/api/v1/settings/routers/get_settings_router.py types "
        "LLMConfigInputDTO.provider as a Literal union that omits 'bedrock', so "
        "Python rejects the value it advertises on GET. "
        ".github/workflows/http-parity.yml pins topoteretes/cognee at "
        "b9014c16, which predates any fix, and P1 cannot be landed from this "
        "repository. strict=True makes this XPASS loudly (a CI failure) the day "
        "upstream adds Literal['bedrock'] and the pin is bumped — at which "
        "point delete this marker rather than the test."
    ),
    strict=True,
)
def test_settings_post_bedrock_accepted_by_python(py_settings_client):
    """POST provider="bedrock" is accepted by Python (xfail: upstream P1).

    No specific rejection status is asserted: cognee/FastAPI's exact
    400-vs-422 mapping for the Literal violation is not verifiable from this
    repository. Asserting the *success* case under a strict xfail covers both
    directions — it fails today and turns red the moment upstream lands P1.

    Unlike the Rust case there is no restore leg: the POST cannot currently
    succeed, and once P1 lands the mutation is confined to this pytest session
    anyway (`start_servers.sh` is the compose entrypoint, so every phase step
    boots its own server pair, and Phase-1a exercises no LLM call).
    """
    r = py_settings_client.post(SETTINGS_PATH, json=_BEDROCK_POST)
    assert r.status_code == 200, (
        f"py: POST {SETTINGS_PATH} provider=bedrock -> {r.status_code}: {r.text[:300]}"
    )
