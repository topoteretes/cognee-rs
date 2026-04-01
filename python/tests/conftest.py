import pytest
import cognee_pipeline as cp


@pytest.fixture
def ctx():
    """Create a fresh mock TaskContext for each test."""
    return cp.TaskContext.mock()
