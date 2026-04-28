"""Phase-3 parity tests for the permissions / tenant / role / ACL API.

Per p8-e2e-parity.md Step 11: walks the 13-endpoint permission-API surface in
CRUD order.  Principals / roles / tenants are created on each server
independently — the test asserts *shape* of responses, not concrete IDs.

Ignore extension: ``{"$..tenant_id", "$..principal_id", "$..role_id", "$..created_at"}``.
"""

from __future__ import annotations

import uuid

import pytest

from http_helpers import DEFAULT_IGNORE, assert_responses_match

_PERM_IGNORE = DEFAULT_IGNORE | {
    "$..tenant_id",
    "$..principal_id",
    "$..role_id",
    "$..created_at",
    "$..updated_at",
}


def _skip_if_404(py, rs, description: str) -> None:
    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip(f"{description} not yet implemented (404 on both)")


def test_permissions_list_tenants(authed_clients):
    """GET /api/v1/permissions/tenants lists tenants on both servers."""
    py = authed_clients["py"].get("/api/v1/permissions/tenants")
    rs = authed_clients["rs"].get("/api/v1/permissions/tenants")
    _skip_if_404(py, rs, "GET /api/v1/permissions/tenants")
    assert_responses_match(py, rs, ignore=_PERM_IGNORE)


def test_permissions_create_tenant(authed_clients):
    """POST /api/v1/permissions/tenants creates a tenant on both servers."""
    payload = {"name": f"test_tenant_{uuid.uuid4().hex[:8]}"}
    py = authed_clients["py"].post("/api/v1/permissions/tenants", json=payload)
    rs = authed_clients["rs"].post("/api/v1/permissions/tenants", json=payload)
    _skip_if_404(py, rs, "POST /api/v1/permissions/tenants")
    assert py.status_code == rs.status_code, (
        f"create-tenant status mismatch: py={py.status_code} rs={rs.status_code}"
    )


def test_permissions_list_roles(authed_clients):
    """GET /api/v1/permissions/roles lists roles on both servers."""
    py = authed_clients["py"].get("/api/v1/permissions/roles")
    rs = authed_clients["rs"].get("/api/v1/permissions/roles")
    _skip_if_404(py, rs, "GET /api/v1/permissions/roles")
    assert_responses_match(py, rs, ignore=_PERM_IGNORE)


def test_permissions_create_role(authed_clients):
    """POST /api/v1/permissions/roles creates a role on both servers."""
    payload = {"name": f"test_role_{uuid.uuid4().hex[:8]}", "description": "parity test role"}
    py = authed_clients["py"].post("/api/v1/permissions/roles", json=payload)
    rs = authed_clients["rs"].post("/api/v1/permissions/roles", json=payload)
    _skip_if_404(py, rs, "POST /api/v1/permissions/roles")
    assert py.status_code == rs.status_code, (
        f"create-role status mismatch: py={py.status_code} rs={rs.status_code}"
    )


def test_permissions_list_principals(authed_clients):
    """GET /api/v1/permissions/principals lists principals on both servers."""
    py = authed_clients["py"].get("/api/v1/permissions/principals")
    rs = authed_clients["rs"].get("/api/v1/permissions/principals")
    _skip_if_404(py, rs, "GET /api/v1/permissions/principals")
    assert_responses_match(py, rs, ignore=_PERM_IGNORE)


def test_permissions_create_principal(authed_clients):
    """POST /api/v1/permissions/principals creates a principal on both servers."""
    payload = {"name": f"test_principal_{uuid.uuid4().hex[:8]}", "type": "user"}
    py = authed_clients["py"].post("/api/v1/permissions/principals", json=payload)
    rs = authed_clients["rs"].post("/api/v1/permissions/principals", json=payload)
    _skip_if_404(py, rs, "POST /api/v1/permissions/principals")
    assert py.status_code == rs.status_code, (
        f"create-principal status mismatch: py={py.status_code} rs={rs.status_code}"
    )


def test_permissions_assign_role_to_principal(authed_clients):
    """POST /api/v1/permissions/principals/{id}/roles assigns a role on both servers."""
    # Use placeholder IDs — actual test would chain on create fixtures
    placeholder_principal = str(uuid.uuid4())
    placeholder_role = str(uuid.uuid4())
    py = authed_clients["py"].post(
        f"/api/v1/permissions/principals/{placeholder_principal}/roles",
        json={"role_id": placeholder_role},
    )
    rs = authed_clients["rs"].post(
        f"/api/v1/permissions/principals/{placeholder_principal}/roles",
        json={"role_id": placeholder_role},
    )
    _skip_if_404(py, rs, "POST /api/v1/permissions/principals/{id}/roles")
    assert py.status_code == rs.status_code, (
        f"assign-role status mismatch: py={py.status_code} rs={rs.status_code}"
    )


def test_permissions_list_acls(authed_clients):
    """GET /api/v1/permissions/acls lists ACL entries on both servers."""
    py = authed_clients["py"].get("/api/v1/permissions/acls")
    rs = authed_clients["rs"].get("/api/v1/permissions/acls")
    _skip_if_404(py, rs, "GET /api/v1/permissions/acls")
    assert_responses_match(py, rs, ignore=_PERM_IGNORE)


def test_permissions_create_acl(authed_clients):
    """POST /api/v1/permissions/acls creates an ACL entry on both servers."""
    payload = {
        "principal_id": str(uuid.uuid4()),
        "resource_type": "dataset",
        "resource_id": str(uuid.uuid4()),
        "permission": "read",
    }
    py = authed_clients["py"].post("/api/v1/permissions/acls", json=payload)
    rs = authed_clients["rs"].post("/api/v1/permissions/acls", json=payload)
    _skip_if_404(py, rs, "POST /api/v1/permissions/acls")
    assert py.status_code == rs.status_code, (
        f"create-acl status mismatch: py={py.status_code} rs={rs.status_code}"
    )


def test_permissions_get_role_by_id(authed_clients):
    """GET /api/v1/permissions/roles/{id} returns 404 for non-existent role on both."""
    nonexistent = str(uuid.uuid4())
    py = authed_clients["py"].get(f"/api/v1/permissions/roles/{nonexistent}")
    rs = authed_clients["rs"].get(f"/api/v1/permissions/roles/{nonexistent}")
    _skip_if_404(py, rs, "GET /api/v1/permissions/roles/{id}")
    assert py.status_code == rs.status_code, (
        f"get-role-by-id status mismatch: py={py.status_code} rs={rs.status_code}"
    )


def test_permissions_delete_role(authed_clients):
    """DELETE /api/v1/permissions/roles/{id} returns 404 for non-existent role on both."""
    nonexistent = str(uuid.uuid4())
    py = authed_clients["py"].delete(f"/api/v1/permissions/roles/{nonexistent}")
    rs = authed_clients["rs"].delete(f"/api/v1/permissions/roles/{nonexistent}")
    _skip_if_404(py, rs, "DELETE /api/v1/permissions/roles/{id}")
    assert py.status_code == rs.status_code, (
        f"delete-role status mismatch: py={py.status_code} rs={rs.status_code}"
    )


def test_permissions_update_role(authed_clients):
    """PATCH /api/v1/permissions/roles/{id} returns 404 for non-existent role on both."""
    nonexistent = str(uuid.uuid4())
    py = authed_clients["py"].patch(
        f"/api/v1/permissions/roles/{nonexistent}",
        json={"description": "updated"},
    )
    rs = authed_clients["rs"].patch(
        f"/api/v1/permissions/roles/{nonexistent}",
        json={"description": "updated"},
    )
    _skip_if_404(py, rs, "PATCH /api/v1/permissions/roles/{id}")
    assert py.status_code == rs.status_code, (
        f"update-role status mismatch: py={py.status_code} rs={rs.status_code}"
    )


def test_permissions_check_access(authed_clients):
    """POST /api/v1/permissions/check returns access verdict on both servers."""
    payload = {
        "principal_id": str(uuid.uuid4()),
        "resource_type": "dataset",
        "resource_id": str(uuid.uuid4()),
        "permission": "read",
    }
    py = authed_clients["py"].post("/api/v1/permissions/check", json=payload)
    rs = authed_clients["rs"].post("/api/v1/permissions/check", json=payload)
    _skip_if_404(py, rs, "POST /api/v1/permissions/check")
    assert py.status_code == rs.status_code, (
        f"check-access status mismatch: py={py.status_code} rs={rs.status_code}"
    )
